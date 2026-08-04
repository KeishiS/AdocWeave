//! Source-backed document structure for natural-language consumers.

use crate::block_model::{
    AstBlock, BlockMetadata, DelimitedBlockKind, DelimitedContent, HeadingKind, ListBlock, ListItem,
};
use crate::core::{Analysis, SourceId};
use crate::inline::{Inline, InlineStyle};
use crate::source::{TextRange, TextSize};
use crate::syntax::SyntaxKind;
use crate::table::{Table, TableCellContent};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextProjection {
    pub package_version: &'static str,
    pub source_id: Option<SourceId>,
    pub source_range: TextRange,
    pub children: Vec<TextNode>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextNode {
    pub kind: TextNodeKind,
    pub source_range: TextRange,
    pub content_range: Option<TextRange>,
    pub level: Option<u8>,
    pub children: Vec<TextNode>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextNodeKind {
    BlockTitle,
    Heading,
    Paragraph,
    List,
    ListItem,
    DescriptionTerm,
    BlockQuote,
    Table,
    TableRow,
    TableCell,
    CodeBlock,
    Comment,
    Text,
    Code,
    Strong,
    Emphasis,
    Link,
    Reference,
    HardBreak,
    Container,
    Excluded,
}

pub fn project_text(analysis: &Analysis) -> TextProjection {
    let source_range = analysis.syntax().root().range();
    let mut children = analysis
        .document()
        .blocks()
        .iter()
        .map(project_block)
        .collect::<Vec<_>>();

    for comment in analysis.syntax().nodes(SyntaxKind::CommentLine) {
        insert_comment(
            &mut children,
            TextNode {
                kind: TextNodeKind::Comment,
                source_range: comment.range(),
                content_range: line_comment_content_range(analysis.source(), comment.range()),
                level: None,
                children: Vec::new(),
            },
        );
    }
    sort_children(&mut children);

    TextProjection {
        package_version: analysis.package_version(),
        source_id: analysis.source_id().cloned(),
        source_range,
        children,
    }
}

fn project_block(block: &AstBlock) -> TextNode {
    let mut node = match block {
        AstBlock::Heading(heading) => TextNode {
            kind: TextNodeKind::Heading,
            source_range: heading.range,
            content_range: Some(heading.text_range),
            level: Some(match heading.kind {
                HeadingKind::DocumentTitle | HeadingKind::Part => 1,
                HeadingKind::Section { level } | HeadingKind::Discrete { level } => level,
            }),
            children: project_inlines(&heading.inlines),
        },
        AstBlock::Paragraph(paragraph) => TextNode {
            kind: TextNodeKind::Paragraph,
            source_range: paragraph.range,
            content_range: Some(paragraph.content_range),
            level: None,
            children: project_inlines(&paragraph.inlines),
        },
        AstBlock::LiteralParagraph(literal) => leaf(
            TextNodeKind::CodeBlock,
            literal.range,
            Some(literal.content_range),
        ),
        AstBlock::Break(value) => leaf(TextNodeKind::Excluded, value.range, None),
        AstBlock::Source(source) => leaf(
            TextNodeKind::CodeBlock,
            source.range,
            Some(source.content_range),
        ),
        AstBlock::Verbatim(verbatim) => leaf(
            TextNodeKind::CodeBlock,
            verbatim.range,
            Some(verbatim.content_range),
        ),
        AstBlock::List(list) => project_list(list),
        AstBlock::Math(math) => leaf(TextNodeKind::Excluded, math.range, Some(math.content_range)),
        AstBlock::Delimited(delimited) => {
            let kind = match delimited.kind {
                DelimitedBlockKind::Comment => TextNodeKind::Comment,
                DelimitedBlockKind::Listing | DelimitedBlockKind::Literal => {
                    TextNodeKind::CodeBlock
                }
                DelimitedBlockKind::Pass => TextNodeKind::Excluded,
                DelimitedBlockKind::Quote => TextNodeKind::BlockQuote,
                DelimitedBlockKind::Table => TextNodeKind::Table,
                DelimitedBlockKind::Example
                | DelimitedBlockKind::Open
                | DelimitedBlockKind::Sidebar => TextNodeKind::Container,
            };
            let children = match &delimited.content {
                DelimitedContent::Compound(blocks) => {
                    blocks.iter().map(project_block).collect::<Vec<_>>()
                }
                DelimitedContent::Table(table) => project_table_children(table),
                DelimitedContent::Verbatim(_) | DelimitedContent::Passthrough(_) => Vec::new(),
            };
            TextNode {
                kind,
                source_range: delimited.range,
                content_range: Some(delimited.content_range),
                level: None,
                children,
            }
        }
        AstBlock::Unsupported(value) => leaf(TextNodeKind::Excluded, value.range, None),
    };
    if !matches!(block, AstBlock::List(_)) {
        prepend_title(&mut node, block.metadata());
    }
    node.source_range = source_range_with_metadata(node.source_range, block.metadata());
    sort_children(&mut node.children);
    node
}

fn project_list(list: &ListBlock) -> TextNode {
    let mut node = TextNode {
        kind: TextNodeKind::List,
        source_range: list.range,
        content_range: None,
        level: None,
        children: list.items.iter().map(project_list_item).collect(),
    };
    prepend_title(&mut node, &list.metadata);
    node.source_range = source_range_with_metadata(node.source_range, &list.metadata);
    sort_children(&mut node.children);
    node
}

fn project_list_item(item: &ListItem) -> TextNode {
    let mut children = Vec::new();
    children.extend(item.terms.iter().map(|term| TextNode {
        kind: TextNodeKind::DescriptionTerm,
        source_range: term.range,
        content_range: Some(term.range),
        level: None,
        children: project_inlines(&term.inlines),
    }));
    children.extend(project_inlines(&item.inlines));
    children.extend(item.children.iter().map(project_list));
    children.extend(item.continuations.iter().map(project_block));
    sort_children(&mut children);
    TextNode {
        kind: TextNodeKind::ListItem,
        source_range: item.range,
        content_range: Some(item.text_range),
        level: None,
        children,
    }
}

fn project_table_children(table: &Table) -> Vec<TextNode> {
    table
        .rows
        .iter()
        .map(|row| TextNode {
            kind: TextNodeKind::TableRow,
            source_range: row.range,
            content_range: None,
            level: None,
            children: row
                .cells
                .iter()
                .map(|cell| {
                    let children = match &cell.content {
                        TableCellContent::Inlines(inlines) => project_inlines(inlines),
                        TableCellContent::AsciiDoc(blocks) => {
                            blocks.iter().map(project_block).collect()
                        }
                        TableCellContent::Verbatim(_) => Vec::new(),
                    };
                    TextNode {
                        kind: match cell.content {
                            TableCellContent::Verbatim(_) => TextNodeKind::CodeBlock,
                            TableCellContent::Inlines(_) | TableCellContent::AsciiDoc(_) => {
                                TextNodeKind::TableCell
                            }
                        },
                        source_range: cell.range,
                        content_range: Some(cell.content_range),
                        level: None,
                        children,
                    }
                })
                .collect(),
        })
        .collect()
}

fn project_inlines(inlines: &[Inline]) -> Vec<TextNode> {
    inlines.iter().map(project_inline).collect()
}

fn project_inline(inline: &Inline) -> TextNode {
    match inline {
        Inline::Text(text) => leaf(TextNodeKind::Text, text.range, Some(text.range)),
        Inline::Literal {
            range,
            content_range,
            ..
        } => leaf(TextNodeKind::Code, *range, Some(*content_range)),
        Inline::Styled {
            style,
            range,
            content_range,
            children,
        } => TextNode {
            kind: match style {
                InlineStyle::Strong => TextNodeKind::Strong,
                InlineStyle::Emphasis => TextNodeKind::Emphasis,
                InlineStyle::Highlight
                | InlineStyle::Subscript
                | InlineStyle::Superscript
                | InlineStyle::CurvedDoubleQuote
                | InlineStyle::CurvedSingleQuote => TextNodeKind::Container,
            },
            source_range: *range,
            content_range: Some(*content_range),
            level: None,
            children: project_inlines(children),
        },
        Inline::AttributeReference { range, .. }
        | Inline::Passthrough { range, .. }
        | Inline::HardBreak { range } => {
            let kind = if matches!(inline, Inline::HardBreak { .. }) {
                TextNodeKind::HardBreak
            } else {
                TextNodeKind::Excluded
            };
            leaf(kind, *range, None)
        }
        Inline::Link(link) => TextNode {
            kind: TextNodeKind::Link,
            source_range: link.range,
            content_range: link.label_range,
            level: None,
            children: project_inlines(&link.label),
        },
        Inline::Reference(reference) => TextNode {
            kind: TextNodeKind::Reference,
            source_range: reference.range,
            content_range: reference.label_range,
            level: None,
            children: project_inlines(&reference.label),
        },
        Inline::Formula(formula) => leaf(
            TextNodeKind::Excluded,
            formula.range,
            Some(formula.content_range),
        ),
        Inline::Macro(node) => leaf(TextNodeKind::Excluded, node.range, None),
    }
}

fn prepend_title(node: &mut TextNode, metadata: &BlockMetadata) {
    let Some(title) = &metadata.title else {
        return;
    };
    node.children.push(TextNode {
        kind: TextNodeKind::BlockTitle,
        source_range: title.range,
        content_range: Some(title.range),
        level: None,
        children: project_inlines(&title.inlines),
    });
}

fn source_range_with_metadata(range: TextRange, metadata: &BlockMetadata) -> TextRange {
    let Some(metadata_range) = metadata.range else {
        return range;
    };
    TextRange::new(
        range.start().min(metadata_range.start()),
        range.end().max(metadata_range.end()),
    )
    .expect("metadata and block ranges are ordered")
}

fn leaf(kind: TextNodeKind, source_range: TextRange, content_range: Option<TextRange>) -> TextNode {
    TextNode {
        kind,
        source_range,
        content_range,
        level: None,
        children: Vec::new(),
    }
}

fn insert_comment(children: &mut Vec<TextNode>, comment: TextNode) {
    if let Some(parent) = children.iter_mut().find(|child| {
        child.source_range.start() <= comment.source_range.start()
            && comment.source_range.end() <= child.source_range.end()
    }) {
        insert_comment(&mut parent.children, comment);
    } else {
        children.push(comment);
    }
}

fn sort_children(children: &mut [TextNode]) {
    children.sort_by_key(|child| (child.source_range.start(), child.source_range.end()));
}

fn line_comment_content_range(source: &str, range: TextRange) -> Option<TextRange> {
    let start = range.start().to_usize();
    let end = range.end().to_usize();
    let raw = source.get(start..end)?;
    let prefix = raw.find("//")? + 2;
    let leading = raw[prefix..]
        .char_indices()
        .find(|(_, character)| !character.is_whitespace())
        .map_or(raw.len(), |(offset, _)| prefix + offset);
    let trailing = raw.trim_end_matches(['\r', '\n']).len();
    TextRange::new(
        TextSize::new(start + leading).ok()?,
        TextSize::new(start + trailing).ok()?,
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AnalysisOptions, Engine};

    fn projection(source: &str) -> TextProjection {
        let analysis = Engine::new(AnalysisOptions::default())
            .analyze(source)
            .expect("analysis");
        project_text(&analysis)
    }

    #[test]
    fn projects_source_backed_document_structure() {
        fn contains(node: &TextNode, kind: TextNodeKind) -> bool {
            node.kind == kind || node.children.iter().any(|child| contains(child, kind))
        }

        let source = "= 表題\n\n// textlint-disable\n\n== 節\n\n段落の**強調**と `code` です。\n\n* 項目\n\n|===\n|セル\n|===\n";
        let projected = projection(source);
        assert_eq!(projected.source_range.end().to_usize(), source.len());
        assert!(
            projected
                .children
                .iter()
                .any(|node| node.kind == TextNodeKind::Comment)
        );
        assert!(
            projected
                .children
                .iter()
                .any(|node| node.kind == TextNodeKind::List)
        );
        assert!(
            projected
                .children
                .iter()
                .any(|node| node.kind == TextNodeKind::Table)
        );
        let paragraph = projected
            .children
            .iter()
            .find(|node| node.kind == TextNodeKind::Paragraph)
            .expect("paragraph");
        assert!(contains(paragraph, TextNodeKind::Strong));
        assert!(contains(paragraph, TextNodeKind::Code));
    }

    #[test]
    fn every_range_is_an_utf8_boundary_and_children_are_ordered() {
        fn check(source: &str, parent: TextRange, children: &[TextNode]) {
            let mut previous = parent.start();
            for child in children {
                assert!(parent.start() <= child.source_range.start());
                assert!(child.source_range.end() <= parent.end());
                assert!(previous <= child.source_range.start());
                assert!(source.is_char_boundary(child.source_range.start().to_usize()));
                assert!(source.is_char_boundary(child.source_range.end().to_usize()));
                if let Some(content) = child.content_range {
                    assert!(child.source_range.start() <= content.start());
                    assert!(content.end() <= child.source_range.end());
                }
                check(source, child.source_range, &child.children);
                previous = child.source_range.end();
            }
        }

        let source = "= 文書😀\r\n\r\n// 指示\r\n\r\n本文の_強調_です。\r\n";
        let projected = projection(source);
        check(source, projected.source_range, &projected.children);
    }
}
