//! Lossless concrete syntax and HTML-independent semantic syntax.

use std::fmt::Write as _;

use crate::attributes::{AttributeProblem, DocumentAttribute, parse_line as parse_attribute_line};
use crate::inline::{Inline, InlineParseConfig, InlineProblem, parse as parse_inlines};
use crate::source::{PositionError, TextRange, TextSize};
use crate::source_lines::{LosslessToken, SourceLine, SourceLines};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CstBlockKind {
    DocumentTitle,
    Heading,
    MalformedHeading,
    Paragraph,
    LiteralBlock,
    SourceBlock,
    BlankLine,
    Unsupported,
    DocumentAttribute,
    BlockAnchor,
    List,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CstBlock {
    pub kind: CstBlockKind,
    pub range: TextRange,
}

#[derive(Debug)]
pub struct CstDocument<'source> {
    source_lines: SourceLines<'source>,
    blocks: Vec<CstBlock>,
}

impl<'source> CstDocument<'source> {
    pub fn source(&self) -> &'source str {
        self.source_lines.source()
    }

    pub fn blocks(&self) -> &[CstBlock] {
        &self.blocks
    }

    pub(crate) const fn source_lines(&self) -> &SourceLines<'source> {
        &self.source_lines
    }

    pub fn tokens(&self) -> &[LosslessToken] {
        self.source_lines.tokens()
    }

    pub fn reconstruct(&self) -> String {
        self.source_lines.reconstruct()
    }

    pub fn snapshot(&self) -> String {
        let mut output = String::new();
        writeln!(output, "Document@0..{}", self.source().len())
            .expect("writing to a String cannot fail");
        for block in &self.blocks {
            writeln!(
                output,
                "  {:?}@{}..{}",
                block.kind,
                block.range.start().to_u32(),
                block.range.end().to_u32()
            )
            .expect("writing to a String cannot fail");
        }
        output
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextNode {
    pub range: TextRange,
    pub value: String,
    pub inlines: Vec<Inline>,
    pub inline_problems: Vec<InlineProblem>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Paragraph {
    pub range: TextRange,
    pub lines: Vec<TextNode>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Unsupported {
    pub range: TextRange,
    pub raw: String,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExplicitAnchor {
    pub range: TextRange,
    pub id_range: TextRange,
    pub label_range: Option<TextRange>,
    pub id: String,
    pub label: Option<String>,
    pub target_range: Option<TextRange>,
    pub valid: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockProblemKind {
    UnclosedBlock,
    MissingSourceLanguage,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlockProblem {
    pub kind: BlockProblemKind,
    pub range: TextRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LiteralBlock {
    pub range: TextRange,
    pub delimiter_range: TextRange,
    pub content_range: TextRange,
    pub value: String,
    pub problems: Vec<BlockProblem>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceBlock {
    pub range: TextRange,
    pub attribute_range: TextRange,
    pub language_range: Option<TextRange>,
    pub language: Option<String>,
    pub delimiter_range: TextRange,
    pub content_range: TextRange,
    pub value: String,
    pub problems: Vec<BlockProblem>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListKind {
    Unordered,
    Ordered,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListProblemKind {
    EmptyItem,
    InconsistentMarker,
    InvalidNesting,
    DepthLimitExceeded,
    NonCanonicalSeparator,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ListProblem {
    pub kind: ListProblemKind,
    pub range: TextRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListBlock {
    pub kind: ListKind,
    pub range: TextRange,
    pub items: Vec<ListItem>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListItem {
    pub range: TextRange,
    pub marker_range: TextRange,
    pub separator_range: TextRange,
    pub text_range: TextRange,
    pub text: String,
    pub inlines: Vec<Inline>,
    pub inline_problems: Vec<InlineProblem>,
    pub children: Vec<ListBlock>,
    pub continuations: Vec<AstBlock>,
    pub continuation_ranges: Vec<TextRange>,
    pub problems: Vec<ListProblem>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeadingKind {
    DocumentTitle,
    Section { level: u8 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeadingProblem {
    MissingSpace,
    EmptyText,
    LevelTooDeep,
    MisplacedDocumentTitle,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Heading {
    pub range: TextRange,
    pub marker_range: TextRange,
    pub separator_range: TextRange,
    pub text_range: TextRange,
    pub kind: HeadingKind,
    pub text: String,
    pub inlines: Vec<Inline>,
    pub inline_problems: Vec<InlineProblem>,
    pub problems: Vec<HeadingProblem>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AstBlock {
    Heading(Heading),
    Paragraph(Paragraph),
    Literal(LiteralBlock),
    Source(SourceBlock),
    List(ListBlock),
    Unsupported(Unsupported),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AstDocument {
    pub blocks: Vec<AstBlock>,
    pub attributes: Vec<DocumentAttribute>,
    pub attribute_problems: Vec<AttributeProblem>,
    pub anchors: Vec<ExplicitAnchor>,
}

impl AstDocument {
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
                    for line in &paragraph.lines {
                        writeln!(
                            output,
                            "    Text@{}..{} {:?}",
                            line.range.start().to_u32(),
                            line.range.end().to_u32(),
                            line.value
                        )
                        .expect("writing to a String cannot fail");
                    }
                }
                AstBlock::Literal(literal) => {
                    writeln!(
                        output,
                        "  Literal@{}..{} content={}..{} {:?} problems={:?}",
                        literal.range.start().to_u32(),
                        literal.range.end().to_u32(),
                        literal.content_range.start().to_u32(),
                        literal.content_range.end().to_u32(),
                        literal.value,
                        literal.problems
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
}

#[derive(Debug)]
pub struct ParsedDocument<'source> {
    pub cst: CstDocument<'source>,
    pub ast: AstDocument,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParseConfig {
    pub max_inline_depth: usize,
    pub max_list_depth: usize,
}

impl Default for ParseConfig {
    fn default() -> Self {
        Self {
            max_inline_depth: 32,
            max_list_depth: 8,
        }
    }
}

pub fn parse(source: &str) -> Result<ParsedDocument<'_>, PositionError> {
    parse_with_config(source, &ParseConfig::default())
}

pub fn parse_with_config<'source>(
    source: &'source str,
    config: &ParseConfig,
) -> Result<ParsedDocument<'source>, PositionError> {
    let source_lines = SourceLines::new(source)?;
    let mut blocks = Vec::new();
    let mut ast_blocks = Vec::new();
    let mut paragraph_lines = Vec::new();
    let mut saw_content = false;
    let mut header_attributes_open = false;
    let mut attributes = Vec::new();
    let mut attribute_problems = Vec::new();
    let mut anchors = Vec::new();

    let mut line_index = 0;
    while line_index < source_lines.lines().len() {
        let line = source_lines.lines()[line_index];
        let content = source_lines
            .text(line.content_range())
            .expect("line content has valid UTF-8 boundaries");

        if parse_source_attribute(content).is_some()
            && source_lines
                .lines()
                .get(line_index + 1)
                .and_then(|next| source_lines.text(next.content_range()))
                == Some("----")
        {
            flush_paragraph(&mut blocks, &mut ast_blocks, &mut paragraph_lines, config);
            let (source_block, next_line) = parse_source_block(&source_lines, line_index, source)?;
            blocks.push(CstBlock {
                kind: CstBlockKind::SourceBlock,
                range: source_block.range,
            });
            ast_blocks.push(AstBlock::Source(source_block));
            saw_content = true;
            line_index = next_line;
            continue;
        } else if content == "...." {
            flush_paragraph(&mut blocks, &mut ast_blocks, &mut paragraph_lines, config);
            let (literal, next_line) = parse_literal_block(&source_lines, line_index, source)?;
            blocks.push(CstBlock {
                kind: CstBlockKind::LiteralBlock,
                range: literal.range,
            });
            ast_blocks.push(AstBlock::Literal(literal));
            saw_content = true;
            line_index = next_line;
            continue;
        } else if let Some(anchor) = parse_explicit_anchor(
            content,
            line.content_range().start().to_usize(),
            line.full_range(),
        ) {
            flush_paragraph(&mut blocks, &mut ast_blocks, &mut paragraph_lines, config);
            blocks.push(CstBlock {
                kind: CstBlockKind::BlockAnchor,
                range: line.full_range(),
            });
            anchors.push(anchor);
            saw_content = true;
            header_attributes_open = false;
        } else if content.trim_matches([' ', '\t']).is_empty() {
            flush_paragraph(&mut blocks, &mut ast_blocks, &mut paragraph_lines, config);
            blocks.push(CstBlock {
                kind: CstBlockKind::BlankLine,
                range: line.full_range(),
            });
            if header_attributes_open {
                header_attributes_open = false;
            }
        } else if header_attributes_open
            && parse_attribute_line(
                content,
                line.content_range().start().to_usize(),
                line.full_range(),
            )
            .is_some()
        {
            flush_paragraph(&mut blocks, &mut ast_blocks, &mut paragraph_lines, config);
            let (attribute, problem) = parse_attribute_line(
                content,
                line.content_range().start().to_usize(),
                line.full_range(),
            )
            .expect("checked above");
            blocks.push(CstBlock {
                kind: CstBlockKind::DocumentAttribute,
                range: line.full_range(),
            });
            attributes.push(attribute);
            attribute_problems.extend(problem);
        } else if content.starts_with('=') {
            flush_paragraph(&mut blocks, &mut ast_blocks, &mut paragraph_lines, config);
            let heading = parse_heading(content, line, !saw_content, config)?;
            blocks.push(CstBlock {
                kind: if heading.problems.is_empty() {
                    match heading.kind {
                        HeadingKind::DocumentTitle => CstBlockKind::DocumentTitle,
                        HeadingKind::Section { .. } => CstBlockKind::Heading,
                    }
                } else {
                    CstBlockKind::MalformedHeading
                },
                range: line.full_range(),
            });
            ast_blocks.push(AstBlock::Heading(heading));
            header_attributes_open = matches!(
                ast_blocks.last(),
                Some(AstBlock::Heading(Heading {
                    kind: HeadingKind::DocumentTitle,
                    ..
                }))
            );
            saw_content = true;
        } else if list_marker(content).is_some() {
            flush_paragraph(&mut blocks, &mut ast_blocks, &mut paragraph_lines, config);
            let (lists, next_line, range) = parse_lists(&source_lines, line_index, source, config)?;
            blocks.push(CstBlock {
                kind: CstBlockKind::List,
                range,
            });
            ast_blocks.extend(lists.into_iter().map(AstBlock::List));
            saw_content = true;
            header_attributes_open = false;
            line_index = next_line;
            continue;
        } else if unsupported_reason(content).is_some() {
            flush_paragraph(&mut blocks, &mut ast_blocks, &mut paragraph_lines, config);
            let reason = unsupported_reason(content).expect("checked above");
            blocks.push(CstBlock {
                kind: CstBlockKind::Unsupported,
                range: line.full_range(),
            });
            ast_blocks.push(AstBlock::Unsupported(Unsupported {
                range: line.full_range(),
                raw: content.to_owned(),
                reason: reason.to_owned(),
            }));
            saw_content = true;
        } else {
            paragraph_lines.push((line, content.trim_end_matches([' ', '\t']).to_owned()));
            saw_content = true;
        }
        line_index += 1;
    }
    flush_paragraph(&mut blocks, &mut ast_blocks, &mut paragraph_lines, config);
    for anchor in &mut anchors {
        anchor.target_range = ast_blocks
            .iter()
            .map(ast_block_range)
            .find(|range| range.start() >= anchor.range.end());
    }
    let mut anchored_targets = std::collections::BTreeSet::new();
    for anchor in &mut anchors {
        if anchor.valid {
            if let Some(target) = anchor.target_range {
                if !anchored_targets.insert((target.start().to_u32(), target.end().to_u32())) {
                    anchor.valid = false;
                }
            } else {
                anchor.valid = false;
            }
        }
    }

    let mut ast = AstDocument {
        blocks: ast_blocks,
        attributes,
        attribute_problems,
        anchors,
    };
    resolve_document_attributes(&mut ast);

    Ok(ParsedDocument {
        cst: CstDocument {
            source_lines,
            blocks,
        },
        ast,
    })
}

fn resolve_document_attributes(document: &mut AstDocument) {
    use crate::attributes::AttributeOperation;
    let mut attributes = std::collections::BTreeMap::new();
    for attribute in &document.attributes {
        match &attribute.operation {
            AttributeOperation::Set(_) => {
                attributes.insert(attribute.name.clone(), attribute.raw_value.clone());
            }
            AttributeOperation::Unset => {
                attributes.remove(&attribute.name);
            }
        }
    }
    fn resolve(inlines: &mut [Inline], attributes: &std::collections::BTreeMap<String, String>) {
        for inline in inlines {
            match inline {
                Inline::Link(link) => {
                    let mut value = String::new();
                    let mut cursor = 0;
                    for attribute in &link.target_attributes {
                        let name_start = attribute.name_range.start().to_usize()
                            - link.target_range.start().to_usize();
                        let name_end = attribute.name_range.end().to_usize()
                            - link.target_range.start().to_usize();
                        let open = name_start.saturating_sub(1);
                        let close_end = (name_end + 1).min(link.target_source.len());
                        value.push_str(&link.target_source[cursor..open]);
                        if let Some(replacement) = attributes.get(&attribute.name) {
                            value.push_str(replacement);
                        } else {
                            value.push_str(&link.target_source[open..close_end]);
                        }
                        cursor = close_end;
                    }
                    value.push_str(&link.target_source[cursor..]);
                    link.target = value;
                    resolve(&mut link.label, attributes);
                }
                Inline::Reference(reference) => resolve(&mut reference.label, attributes),
                Inline::Styled { children, .. } => resolve(children, attributes),
                _ => {}
            }
        }
    }
    for block in &mut document.blocks {
        match block {
            AstBlock::Heading(heading) => resolve(&mut heading.inlines, &attributes),
            AstBlock::Paragraph(paragraph) => {
                for line in &mut paragraph.lines {
                    resolve(&mut line.inlines, &attributes);
                }
            }
            AstBlock::List(list) => resolve_list(list, &attributes),
            _ => {}
        }
    }

    fn resolve_list(list: &mut ListBlock, attributes: &std::collections::BTreeMap<String, String>) {
        for item in &mut list.items {
            resolve(&mut item.inlines, attributes);
            for child in &mut item.children {
                resolve_list(child, attributes);
            }
        }
    }
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

fn valid_anchor_id(id: &str) -> bool {
    !id.is_empty()
        && id.chars().all(|character| {
            !character.is_control()
                && !character.is_whitespace()
                && !matches!(character, '[' | ']' | '<' | '>' | ',' | '#')
        })
}

fn ast_block_range(block: &AstBlock) -> TextRange {
    match block {
        AstBlock::Heading(value) => value.range,
        AstBlock::Paragraph(value) => value.range,
        AstBlock::Literal(value) => value.range,
        AstBlock::Source(value) => value.range,
        AstBlock::List(value) => value.range,
        AstBlock::Unsupported(value) => value.range,
    }
}

#[derive(Debug)]
struct FlatListItem {
    depth: usize,
    kind: ListKind,
    item: ListItem,
}

fn list_marker(content: &str) -> Option<(ListKind, usize, usize)> {
    let marker = content.as_bytes().first().copied()?;
    let kind = match marker {
        b'*' => ListKind::Unordered,
        b'.' => ListKind::Ordered,
        _ => return None,
    };
    let depth = content.bytes().take_while(|byte| *byte == marker).count();
    let separator = *content.as_bytes().get(depth)?;
    matches!(separator, b' ' | b'\t').then_some((kind, depth, depth + 1))
}

fn parse_lists(
    source_lines: &SourceLines<'_>,
    start: usize,
    source: &str,
    config: &ParseConfig,
) -> Result<(Vec<ListBlock>, usize, TextRange), PositionError> {
    let mut flat = Vec::new();
    let mut index = start;
    let mut previous: Option<(usize, ListKind)> = None;
    let mut kinds_by_depth = Vec::<Option<ListKind>>::new();
    while index < source_lines.lines().len() {
        let line = source_lines.lines()[index];
        let content = source_lines.text(line.content_range()).expect("valid line");
        let Some((kind, depth, text_start)) = list_marker(content) else {
            break;
        };
        let absolute = line.content_range().start().to_usize();
        let text = &content[text_start..];
        let marker_range = text_range(absolute, absolute + depth)?;
        let separator_range = text_range(absolute + depth, absolute + text_start)?;
        let text_range = text_range(absolute + text_start, absolute + content.len())?;
        let parsed = parse_inlines(
            text,
            text_range,
            InlineParseConfig {
                max_depth: config.max_inline_depth,
            },
        );
        let mut problems = Vec::new();
        if text.is_empty() {
            problems.push(ListProblem {
                kind: ListProblemKind::EmptyItem,
                range: text_range,
            });
        }
        if content.as_bytes()[depth] == b'\t' {
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
            if depth > previous_depth + 1 {
                problems.push(ListProblem {
                    kind: ListProblemKind::InvalidNesting,
                    range: marker_range,
                });
            }
        }
        if kinds_by_depth
            .get(depth)
            .and_then(|kind| *kind)
            .is_some_and(|established| established != kind)
        {
            problems.push(ListProblem {
                kind: ListProblemKind::InconsistentMarker,
                range: marker_range,
            });
        }
        kinds_by_depth.resize(kinds_by_depth.len().max(depth + 1), None);
        kinds_by_depth[depth] = Some(kind);
        let mut item = ListItem {
            range: line.full_range(),
            marker_range,
            separator_range,
            text_range,
            text: text.to_owned(),
            inlines: parsed.inlines,
            inline_problems: parsed.problems,
            children: Vec::new(),
            continuations: Vec::new(),
            continuation_ranges: Vec::new(),
            problems,
        };
        index += 1;
        while source_lines
            .lines()
            .get(index)
            .is_some_and(|next| source_lines.text(next.content_range()) == Some("+"))
        {
            let continuation = source_lines.lines()[index];
            let next = index + 1;
            let parsed_block = source_lines.lines().get(next).and_then(|next_line| {
                let next_text = source_lines.text(next_line.content_range())?;
                if next_text == "...." {
                    parse_literal_block(source_lines, next, source)
                        .ok()
                        .map(|(block, end)| (AstBlock::Literal(block), end))
                } else if parse_source_attribute(next_text).is_some()
                    && source_lines
                        .lines()
                        .get(next + 1)
                        .and_then(|line| source_lines.text(line.content_range()))
                        == Some("----")
                {
                    parse_source_block(source_lines, next, source)
                        .ok()
                        .map(|(block, end)| (AstBlock::Source(block), end))
                } else {
                    None
                }
            });
            let Some((block, end)) = parsed_block else {
                break;
            };
            item.continuation_ranges.push(continuation.full_range());
            item.range = TextRange::new(item.range.start(), ast_block_range(&block).end())?;
            item.continuations.push(block);
            index = end;
        }
        previous = Some((depth, kind));
        flat.push(FlatListItem { depth, kind, item });
    }
    let end = flat
        .last()
        .map_or(source_lines.lines()[start].full_range().end(), |item| {
            item.item.range.end()
        });
    let range = TextRange::new(source_lines.lines()[start].full_range().start(), end)?;
    let mut cursor = 0;
    let mut roots = Vec::new();
    while cursor < flat.len() {
        let depth = flat[cursor].depth;
        let kind = flat[cursor].kind;
        roots.push(build_list_tree(&mut flat, &mut cursor, depth, kind)?);
    }
    Ok((roots, index, range))
}

fn build_list_tree(
    flat: &mut [FlatListItem],
    cursor: &mut usize,
    depth: usize,
    kind: ListKind,
) -> Result<ListBlock, PositionError> {
    let mut items = Vec::new();
    while *cursor < flat.len() && flat[*cursor].depth == depth && flat[*cursor].kind == kind {
        let mut item = flat[*cursor].item.clone();
        *cursor += 1;
        while *cursor < flat.len() && flat[*cursor].depth > depth {
            let child_depth = flat[*cursor].depth;
            let child_kind = flat[*cursor].kind;
            item.children
                .push(build_list_tree(flat, cursor, child_depth, child_kind)?);
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
    Ok(ListBlock { kind, range, items })
}

fn parse_literal_block(
    source_lines: &SourceLines<'_>,
    opener_index: usize,
    source: &str,
) -> Result<(LiteralBlock, usize), PositionError> {
    let opener = source_lines.lines()[opener_index];
    let body = parse_delimited_body(source_lines, opener_index, "....", source)?;
    let value = source
        .get(body.content_range.start().to_usize()..body.content_range.end().to_usize())
        .expect("literal content range has valid UTF-8 boundaries")
        .to_owned();

    Ok((
        LiteralBlock {
            range: TextRange::new(opener.full_range().start(), body.range_end)?,
            delimiter_range: opener.content_range(),
            content_range: body.content_range,
            value,
            problems: body.problems,
        },
        body.next_line,
    ))
}

struct DelimitedBody {
    range_end: TextSize,
    content_range: TextRange,
    next_line: usize,
    problems: Vec<BlockProblem>,
}

fn parse_delimited_body(
    source_lines: &SourceLines<'_>,
    opener_index: usize,
    delimiter: &str,
    source: &str,
) -> Result<DelimitedBody, PositionError> {
    let opener = source_lines.lines()[opener_index];
    let content_start = opener.full_range().end();
    let mut closer_index = None;
    let mut recovery_index = None;

    for (index, line) in source_lines
        .lines()
        .iter()
        .enumerate()
        .skip(opener_index + 1)
    {
        let content = source_lines
            .text(line.content_range())
            .expect("line content has valid UTF-8 boundaries");
        if content == delimiter {
            closer_index = Some(index);
            break;
        }
        if content.starts_with('=') {
            recovery_index = Some(index);
            break;
        }
    }

    let (range_end, content_end, next_line, problems) = if let Some(index) = closer_index {
        let closer = source_lines.lines()[index];
        (
            closer.full_range().end(),
            closer.full_range().start(),
            index + 1,
            Vec::new(),
        )
    } else {
        let end = recovery_index
            .map(|index| source_lines.lines()[index].full_range().start())
            .unwrap_or_else(|| TextSize::new(source.len()).expect("source size was validated"));
        (
            end,
            end,
            recovery_index.unwrap_or(source_lines.lines().len()),
            vec![BlockProblem {
                kind: BlockProblemKind::UnclosedBlock,
                range: opener.content_range(),
            }],
        )
    };
    Ok(DelimitedBody {
        range_end,
        content_range: TextRange::new(content_start, content_end)?,
        next_line,
        problems,
    })
}

fn parse_source_block(
    source_lines: &SourceLines<'_>,
    attribute_index: usize,
    source: &str,
) -> Result<(SourceBlock, usize), PositionError> {
    let attribute = source_lines.lines()[attribute_index];
    let attribute_text = source_lines
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
    let delimiter = source_lines.lines()[delimiter_index];
    let mut body = parse_delimited_body(source_lines, delimiter_index, "----", source)?;
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

    Ok((
        SourceBlock {
            range: TextRange::new(attribute.full_range().start(), body.range_end)?,
            attribute_range: attribute.content_range(),
            language_range,
            language,
            delimiter_range: delimiter.content_range(),
            content_range: body.content_range,
            value,
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
) -> Result<Heading, PositionError> {
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
        },
    );
    Ok(Heading {
        range: line.full_range(),
        marker_range,
        separator_range,
        text_range,
        kind,
        text: text.to_owned(),
        inlines: inline_output.inlines,
        inline_problems: inline_output.problems,
        problems,
    })
}

fn flush_paragraph(
    cst_blocks: &mut Vec<CstBlock>,
    ast_blocks: &mut Vec<AstBlock>,
    lines: &mut Vec<(SourceLine, String)>,
    config: &ParseConfig,
) {
    let (Some((first, _)), Some((last, _))) = (lines.first(), lines.last()) else {
        return;
    };
    let range = TextRange::new(first.full_range().start(), last.full_range().end())
        .expect("ordered source lines form an ordered paragraph");
    cst_blocks.push(CstBlock {
        kind: CstBlockKind::Paragraph,
        range,
    });
    ast_blocks.push(AstBlock::Paragraph(Paragraph {
        range,
        lines: lines
            .drain(..)
            .map(|(line, value)| {
                let value_range = TextRange::new(
                    line.content_range().start(),
                    crate::source::TextSize::new(
                        line.content_range().start().to_usize() + value.len(),
                    )
                    .expect("source offset fits"),
                )
                .expect("trimmed text range is ordered");
                let inline_output = parse_inlines(
                    &value,
                    value_range,
                    InlineParseConfig {
                        max_depth: config.max_inline_depth,
                    },
                );
                TextNode {
                    range: value_range,
                    inlines: inline_output.inlines,
                    inline_problems: inline_output.problems,
                    value,
                }
            })
            .collect(),
    }));
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
    use super::{AstBlock, BlockProblemKind, CstBlockKind, HeadingKind, HeadingProblem, parse};
    use crate::attributes::{AttributeOperation, AttributeValue, StemKind};

    #[test]
    fn paragraph_parser_handles_empty_input() {
        let parsed = parse("").expect("valid source");

        assert!(parsed.ast.blocks.is_empty());
        assert_eq!(parsed.cst.blocks().len(), 1);
        assert_eq!(parsed.cst.blocks()[0].kind, CstBlockKind::BlankLine);
        assert_eq!(parsed.cst.reconstruct(), "");
    }

    #[test]
    fn document_attributes_preserve_cst_and_produce_typed_ast() {
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

        assert_eq!(parsed.cst.reconstruct(), source);
        assert_eq!(parsed.ast.attributes.len(), 5);
        assert!(matches!(
            parsed.ast.attributes[0].operation,
            AttributeOperation::Set(AttributeValue::Uuid(ref value))
                if value == "123e4567-e89b-12d3-a456-426614174000"
        ));
        assert!(matches!(
            parsed.ast.attributes[2].operation,
            AttributeOperation::Set(AttributeValue::Tags(ref tags))
                if tags == &["rust", "AsciiDoc"]
        ));
        assert_eq!(
            parsed.ast.attributes[3].operation,
            AttributeOperation::Set(AttributeValue::Stem(StemKind::LatexMath))
        );
        assert_eq!(
            parsed.ast.attributes[4].operation,
            AttributeOperation::Unset
        );
        assert!(parsed.ast.attribute_problems.is_empty());
    }

    #[test]
    fn document_attributes_recover_from_incomplete_values() {
        let parsed = parse("= Note\n:note-id:\n:tags:\n\nbody\n").expect("recover");
        assert_eq!(parsed.ast.attributes.len(), 2);
        assert_eq!(parsed.ast.attribute_problems.len(), 2);
        assert!(matches!(
            parsed.ast.blocks.last(),
            Some(AstBlock::Paragraph(_))
        ));
    }

    #[test]
    fn paragraph_parser_groups_lines_and_splits_on_blank_lines() {
        let source = "\nfirst line\nsecond line\n \t\nlast";
        let parsed = parse(source).expect("valid source");

        assert_eq!(parsed.ast.blocks.len(), 2);
        let AstBlock::Paragraph(first) = &parsed.ast.blocks[0] else {
            panic!("expected paragraph");
        };
        assert_eq!(
            first
                .lines
                .iter()
                .map(|line| line.value.as_str())
                .collect::<Vec<_>>(),
            ["first line", "second line"]
        );
        assert_eq!(parsed.cst.reconstruct().as_bytes(), source.as_bytes());
    }

    #[test]
    fn paragraph_parser_keeps_unsupported_syntax_explicit() {
        let source = "before\n\n[role=test]\n\nafter";
        let parsed = parse(source).expect("valid source");

        assert_eq!(parsed.ast.blocks.len(), 3);
        let AstBlock::Unsupported(unsupported) = &parsed.ast.blocks[1] else {
            panic!("expected unsupported node");
        };
        assert_eq!(unsupported.raw, "[role=test]");
        assert_eq!(unsupported.reason, "block attributes are not implemented");
        assert_eq!(parsed.cst.reconstruct(), source);
    }

    #[test]
    fn literal_block_preserves_empty_and_multiline_contents() {
        let source = "....\n<tag>\n*not strong*\n....\n\n....\n....\n";
        let parsed = parse(source).expect("valid source");
        let literals = parsed
            .ast
            .blocks
            .iter()
            .filter_map(|block| match block {
                AstBlock::Literal(literal) => Some(literal),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(literals.len(), 2);
        assert_eq!(literals[0].value, "<tag>\n*not strong*\n");
        assert_eq!(literals[1].value, "");
        assert!(literals.iter().all(|literal| literal.problems.is_empty()));
        assert_eq!(parsed.cst.reconstruct(), source);
    }

    #[test]
    fn literal_block_recovers_at_heading_when_unclosed() {
        let source = "....\ncontent\n== Next\nparagraph";
        let parsed = parse(source).expect("valid source");
        let AstBlock::Literal(literal) = &parsed.ast.blocks[0] else {
            panic!("expected literal");
        };

        assert_eq!(literal.value, "content\n");
        assert_eq!(literal.problems[0].kind, BlockProblemKind::UnclosedBlock);
        assert!(matches!(parsed.ast.blocks[1], AstBlock::Heading(_)));
        assert!(matches!(parsed.ast.blocks[2], AstBlock::Paragraph(_)));
    }

    #[test]
    fn source_block_keeps_language_code_and_ranges() {
        let source = "[source, rust]\n----\nfn main() {}\n----\n";
        let parsed = parse(source).expect("valid source");
        let AstBlock::Source(block) = &parsed.ast.blocks[0] else {
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
        assert_eq!(parsed.cst.reconstruct(), source);
    }

    #[test]
    fn source_block_handles_missing_language_empty_and_unclosed() {
        let parsed = parse("[source]\n----\n== Next\n").expect("valid source");
        let AstBlock::Source(block) = &parsed.ast.blocks[0] else {
            panic!("expected source block");
        };

        assert!(block.language.is_none());
        assert_eq!(block.value, "");
        assert!(
            block
                .problems
                .iter()
                .any(|problem| { problem.kind == BlockProblemKind::MissingSourceLanguage })
        );
        assert!(
            block
                .problems
                .iter()
                .any(|problem| { problem.kind == BlockProblemKind::UnclosedBlock })
        );
        assert!(matches!(parsed.ast.blocks[1], AstBlock::Heading(_)));
    }

    #[test]
    fn heading_parser_distinguishes_title_and_levels_one_to_five() {
        let source = "= Title\n\n== One\n=== Two\n==== Three\n===== Four\n====== Five\n";
        let parsed = parse(source).expect("valid source");
        let headings = parsed
            .ast
            .blocks
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
        let AstBlock::Heading(heading) = &parsed.ast.blocks[0] else {
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
        let AstBlock::Heading(first) = &parsed.ast.blocks[0] else {
            panic!("expected malformed heading");
        };
        assert!(first.problems.contains(&HeadingProblem::MissingSpace));
        let AstBlock::Heading(second) = &parsed.ast.blocks[1] else {
            panic!("expected malformed heading");
        };
        assert!(second.problems.contains(&HeadingProblem::LevelTooDeep));
        assert!(matches!(parsed.ast.blocks[2], AstBlock::Paragraph(_)));
    }

    #[test]
    fn paragraph_parser_matches_cst_and_ast_fixtures() {
        let source = include_str!("../../../fixtures/paragraph/basic.adoc");
        let parsed = parse(source).expect("valid source");

        assert_eq!(
            parsed.cst.snapshot(),
            include_str!("../../../fixtures/paragraph/basic.cst")
        );
        assert_eq!(
            parsed.ast.snapshot(),
            include_str!("../../../fixtures/paragraph/basic.ast")
        );
    }

    #[test]
    fn lists_build_recursive_semantic_nodes() {
        let parsed = parse("* one\n** nested\n* two\n. ordered\n").expect("parse");
        let AstBlock::List(unordered) = &parsed.ast.blocks[0] else {
            panic!("unordered list");
        };
        assert_eq!(unordered.items.len(), 2);
        assert_eq!(unordered.items[0].children[0].items[0].text, "nested");
        assert!(matches!(parsed.ast.blocks[1], AstBlock::List(_)));
    }

    #[test]
    fn list_continuation_attaches_literal_and_source_blocks() {
        let source =
            "* item\n+\n....\nliteral\n....\n* code\n+\n[source,rust]\n----\nfn main() {}\n----\n";
        let parsed = parse(source).expect("parse");
        let AstBlock::List(list) = &parsed.ast.blocks[0] else {
            panic!("list");
        };
        assert!(matches!(
            list.items[0].continuations[0],
            AstBlock::Literal(_)
        ));
        assert!(matches!(
            list.items[1].continuations[0],
            AstBlock::Source(_)
        ));
    }
}
