//! Shared immutable traversal of the output-independent semantic tree.

use std::ops::ControlFlow;

use crate::attributes::DocumentAttributeOccurrence;
use crate::inline::Inline;
use crate::parser::{
    AstBlock, AstDocument, BlockMetadata, BlockTitle, ElementAttribute, ExplicitAnchor, ListBlock,
    ListItem, MetadataValue,
};

#[derive(Clone, Copy, Debug)]
pub enum SemanticNode<'document> {
    Block(&'document AstBlock),
    List(&'document ListBlock),
    ListItem(&'document ListItem),
    Table(&'document crate::table::Table),
    TableRow(&'document crate::table::TableRow),
    TableCell(&'document crate::table::TableCell),
    Inline(&'document Inline),
    Attribute(&'document DocumentAttributeOccurrence),
    Anchor(&'document ExplicitAnchor),
    Metadata(&'document BlockMetadata),
    MetadataTitle(&'document BlockTitle),
    MetadataId(&'document MetadataValue),
    MetadataRole(&'document MetadataValue),
    MetadataOption(&'document MetadataValue),
    ElementAttribute(&'document ElementAttribute),
}

pub fn walk<'document>(
    document: &'document crate::document::Document,
    mut visitor: impl FnMut(SemanticNode<'document>),
) {
    let _: ControlFlow<()> = try_walk_ast(document.inner(), |node| {
        visitor(node);
        ControlFlow::Continue(())
    });
}

pub(crate) fn walk_ast<'document>(
    document: &'document AstDocument,
    mut visitor: impl FnMut(SemanticNode<'document>),
) {
    let _: ControlFlow<()> = try_walk_ast(document, |node| {
        visitor(node);
        ControlFlow::Continue(())
    });
}

/// Traverses the immutable semantic tree until the visitor returns `Break`.
///
/// The node that returns `Break` is visited, but none of its descendants or
/// following siblings are visited. The payload is preserved through every
/// recursive traversal helper.
pub(crate) fn try_walk_ast<'document, Break>(
    document: &'document AstDocument,
    mut visitor: impl FnMut(SemanticNode<'document>) -> ControlFlow<Break>,
) -> ControlFlow<Break> {
    for attribute in document.attributes() {
        visitor(SemanticNode::Attribute(attribute))?;
    }
    for anchor in document.anchors() {
        visitor(SemanticNode::Anchor(anchor))?;
    }
    try_walk_blocks(document.blocks(), &mut visitor)
}

pub(crate) fn walk_block_slice<'document>(
    blocks: &'document [AstBlock],
    mut visitor: impl FnMut(SemanticNode<'document>),
) {
    let _: ControlFlow<()> = try_walk_block_slice(blocks, |node| {
        visitor(node);
        ControlFlow::Continue(())
    });
}

/// Traverses only the supplied blocks, without document attributes or anchors.
pub(crate) fn try_walk_block_slice<'document, Break>(
    blocks: &'document [AstBlock],
    mut visitor: impl FnMut(SemanticNode<'document>) -> ControlFlow<Break>,
) -> ControlFlow<Break> {
    try_walk_blocks(blocks, &mut visitor)
}

pub(crate) trait BlockVisitorMut {
    fn visit_block(&mut self, block: &mut AstBlock);

    fn visit_list(&mut self, _list: &mut ListBlock) {}
}

impl<F> BlockVisitorMut for F
where
    F: FnMut(&mut AstBlock),
{
    fn visit_block(&mut self, block: &mut AstBlock) {
        self(block);
    }
}

pub(crate) fn walk_blocks_mut_cancellable(
    blocks: &mut [AstBlock],
    visitor: &mut impl BlockVisitorMut,
    checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
) -> Result<(), ()> {
    struct BlockAdapter<'visitor, Visitor>(&'visitor mut Visitor);

    impl<Visitor: BlockVisitorMut> SemanticVisitorMut for BlockAdapter<'_, Visitor> {
        fn visit_block(&mut self, block: &mut AstBlock) {
            self.0.visit_block(block);
        }

        fn visit_list(&mut self, list: &mut ListBlock) {
            self.0.visit_list(list);
        }
    }

    walk_semantic_mut_cancellable(blocks, &mut BlockAdapter(visitor), checkpoint)
}

pub(crate) fn walk_inline_sequences_mut_cancellable(
    blocks: &mut [AstBlock],
    visitor: &mut impl FnMut(
        &mut [Inline],
        &mut crate::cancellation::CancellationCheckpoint<'_>,
    ) -> Result<(), ()>,
    checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
) -> Result<(), ()> {
    fn walk_list(
        list: &mut ListBlock,
        visitor: &mut impl FnMut(
            &mut [Inline],
            &mut crate::cancellation::CancellationCheckpoint<'_>,
        ) -> Result<(), ()>,
        checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
    ) -> Result<(), ()> {
        for item in &mut list.items {
            if checkpoint.is_cancelled() {
                return Err(());
            }
            for term in &mut item.terms {
                visitor(&mut term.inlines, checkpoint)?;
            }
            visitor(&mut item.inlines, checkpoint)?;
            for child in &mut item.children {
                walk_list(child, visitor, checkpoint)?;
            }
            walk_blocks(&mut item.continuations, visitor, checkpoint)?;
        }
        Ok(())
    }

    fn walk_blocks(
        blocks: &mut [AstBlock],
        visitor: &mut impl FnMut(
            &mut [Inline],
            &mut crate::cancellation::CancellationCheckpoint<'_>,
        ) -> Result<(), ()>,
        checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
    ) -> Result<(), ()> {
        for block in blocks {
            if checkpoint.is_cancelled() {
                return Err(());
            }
            if let Some(title) = &mut block.metadata_mut().title {
                visitor(&mut title.inlines, checkpoint)?;
            }
            match block {
                AstBlock::Heading(heading) => visitor(&mut heading.inlines, checkpoint)?,
                AstBlock::Paragraph(paragraph) => visitor(&mut paragraph.inlines, checkpoint)?,
                AstBlock::List(list) => walk_list(list, visitor, checkpoint)?,
                AstBlock::Delimited(block) => match &mut block.content {
                    crate::parser::DelimitedContent::Compound(children) => {
                        walk_blocks(children, visitor, checkpoint)?;
                    }
                    crate::parser::DelimitedContent::Table(table) => {
                        for row in &mut table.rows {
                            if checkpoint.is_cancelled() {
                                return Err(());
                            }
                            for cell in &mut row.cells {
                                if checkpoint.is_cancelled() {
                                    return Err(());
                                }
                                match &mut cell.content {
                                    crate::table::TableCellContent::Inlines(inlines) => {
                                        visitor(inlines, checkpoint)?;
                                    }
                                    crate::table::TableCellContent::AsciiDoc(children) => {
                                        walk_blocks(children, visitor, checkpoint)?;
                                    }
                                    crate::table::TableCellContent::Verbatim(_) => {}
                                }
                            }
                        }
                    }
                    crate::parser::DelimitedContent::Verbatim(_)
                    | crate::parser::DelimitedContent::Passthrough(_) => {}
                },
                AstBlock::LiteralParagraph(_)
                | AstBlock::Break(_)
                | AstBlock::Source(_)
                | AstBlock::Verbatim(_)
                | AstBlock::Math(_)
                | AstBlock::Unsupported(_) => {}
            }
        }
        Ok(())
    }

    walk_blocks(blocks, visitor, checkpoint)
}

/// Mutable counterpart of the immutable child topology.
///
/// Adapters select the mutable node categories they need, while this function
/// owns the structural recursion for lists, continuations, compound blocks,
/// tables, AsciiDoc cells, and inline-bearing fields.
trait SemanticVisitorMut {
    fn visit_block(&mut self, _block: &mut AstBlock) {}

    fn visit_list(&mut self, _list: &mut ListBlock) {}

    fn visit_inline_sequence(&mut self, _inlines: &mut Vec<Inline>) {}
}

fn walk_semantic_mut_cancellable(
    blocks: &mut [AstBlock],
    visitor: &mut impl SemanticVisitorMut,
    checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
) -> Result<(), ()> {
    fn walk_list_mut(
        list: &mut ListBlock,
        visitor: &mut impl SemanticVisitorMut,
        checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
    ) -> Result<(), ()> {
        if checkpoint.is_cancelled() {
            return Err(());
        }
        visitor.visit_list(list);
        for item in &mut list.items {
            if checkpoint.is_cancelled() {
                return Err(());
            }
            for term in &mut item.terms {
                if checkpoint.is_cancelled() {
                    return Err(());
                }
                visitor.visit_inline_sequence(&mut term.inlines);
            }
            visitor.visit_inline_sequence(&mut item.inlines);
            for child in &mut item.children {
                walk_list_mut(child, visitor, checkpoint)?;
            }
            walk_semantic_mut_cancellable(&mut item.continuations, visitor, checkpoint)?;
        }
        Ok(())
    }

    for block in blocks {
        if checkpoint.is_cancelled() {
            return Err(());
        }
        visitor.visit_block(block);
        if let Some(title) = &mut block.metadata_mut().title {
            visitor.visit_inline_sequence(&mut title.inlines);
        }
        match block {
            AstBlock::Heading(heading) => visitor.visit_inline_sequence(&mut heading.inlines),
            AstBlock::Paragraph(paragraph) => visitor.visit_inline_sequence(&mut paragraph.inlines),
            AstBlock::List(list) => walk_list_mut(list, visitor, checkpoint)?,
            AstBlock::Delimited(block) => match &mut block.content {
                crate::parser::DelimitedContent::Compound(children) => {
                    walk_semantic_mut_cancellable(children, visitor, checkpoint)?;
                }
                crate::parser::DelimitedContent::Table(table) => {
                    for row in &mut table.rows {
                        if checkpoint.is_cancelled() {
                            return Err(());
                        }
                        for cell in &mut row.cells {
                            if checkpoint.is_cancelled() {
                                return Err(());
                            }
                            match &mut cell.content {
                                crate::table::TableCellContent::Inlines(inlines) => {
                                    visitor.visit_inline_sequence(inlines);
                                }
                                crate::table::TableCellContent::AsciiDoc(children) => {
                                    walk_semantic_mut_cancellable(children, visitor, checkpoint)?;
                                }
                                crate::table::TableCellContent::Verbatim(_) => {}
                            }
                        }
                    }
                }
                crate::parser::DelimitedContent::Verbatim(_)
                | crate::parser::DelimitedContent::Passthrough(_) => {}
            },
            AstBlock::LiteralParagraph(_)
            | AstBlock::Break(_)
            | AstBlock::Source(_)
            | AstBlock::Verbatim(_)
            | AstBlock::Math(_)
            | AstBlock::Unsupported(_) => {}
        }
    }
    Ok(())
}

fn try_walk_blocks<'document, Break>(
    blocks: &'document [AstBlock],
    visitor: &mut impl FnMut(SemanticNode<'document>) -> ControlFlow<Break>,
) -> ControlFlow<Break> {
    for block in blocks {
        try_walk_node(SemanticNode::Block(block), visitor)?;
    }
    ControlFlow::Continue(())
}

fn try_walk_node<'document, Break>(
    node: SemanticNode<'document>,
    visitor: &mut impl FnMut(SemanticNode<'document>) -> ControlFlow<Break>,
) -> ControlFlow<Break> {
    visitor(node)?;
    try_visit_children(node, &mut |child| try_walk_node(child, visitor))
}

/// Visits the immediate semantic children of one node in source event order.
///
/// This is the immutable child-topology contract. Recursive walkers and
/// hierarchy-building consumers must derive descent from this function rather
/// than repeating matches over block, list, table, or inline variants.
pub(crate) fn try_visit_children<'document, Break>(
    node: SemanticNode<'document>,
    visitor: &mut impl FnMut(SemanticNode<'document>) -> ControlFlow<Break>,
) -> ControlFlow<Break> {
    match node {
        SemanticNode::Block(block) => {
            visitor(SemanticNode::Metadata(block.metadata()))?;
            match block {
                AstBlock::Heading(heading) => {
                    visit_inline_children(&heading.inlines, visitor)?;
                }
                AstBlock::Paragraph(paragraph) => {
                    visit_inline_children(&paragraph.inlines, visitor)?;
                }
                AstBlock::List(list) => visitor(SemanticNode::List(list))?,
                AstBlock::Delimited(block) => match &block.content {
                    crate::parser::DelimitedContent::Compound(children) => {
                        visit_block_children(children, visitor)?;
                    }
                    crate::parser::DelimitedContent::Table(table) => {
                        visitor(SemanticNode::Table(table))?;
                    }
                    crate::parser::DelimitedContent::Verbatim(_)
                    | crate::parser::DelimitedContent::Passthrough(_) => {}
                },
                AstBlock::LiteralParagraph(_)
                | AstBlock::Break(_)
                | AstBlock::Source(_)
                | AstBlock::Verbatim(_)
                | AstBlock::Math(_)
                | AstBlock::Unsupported(_) => {}
            }
        }
        SemanticNode::List(list) => {
            for item in &list.items {
                visitor(SemanticNode::ListItem(item))?;
            }
        }
        SemanticNode::ListItem(item) => {
            for term in &item.terms {
                visit_inline_children(&term.inlines, visitor)?;
            }
            visit_inline_children(&item.inlines, visitor)?;
            for child in &item.children {
                visitor(SemanticNode::List(child))?;
            }
            visit_block_children(&item.continuations, visitor)?;
        }
        SemanticNode::Table(table) => {
            for row in &table.rows {
                visitor(SemanticNode::TableRow(row))?;
            }
        }
        SemanticNode::TableRow(row) => {
            for cell in &row.cells {
                visitor(SemanticNode::TableCell(cell))?;
            }
        }
        SemanticNode::TableCell(cell) => match &cell.content {
            crate::table::TableCellContent::Inlines(inlines) => {
                visit_inline_children(inlines, visitor)?;
            }
            crate::table::TableCellContent::AsciiDoc(blocks) => {
                visit_block_children(blocks, visitor)?;
            }
            crate::table::TableCellContent::Verbatim(_) => {}
        },
        SemanticNode::Inline(inline) => match inline {
            Inline::Styled { children, .. } => visit_inline_children(children, visitor)?,
            Inline::Link(link) => visit_inline_children(&link.label, visitor)?,
            Inline::Reference(reference) => visit_inline_children(&reference.label, visitor)?,
            Inline::Macro(_)
            | Inline::Text(_)
            | Inline::Literal { .. }
            | Inline::AttributeReference { .. }
            | Inline::HardBreak { .. }
            | Inline::Passthrough { .. }
            | Inline::Formula(_) => {}
        },
        SemanticNode::Metadata(metadata) => {
            if let Some(title) = &metadata.title {
                visitor(SemanticNode::MetadataTitle(title))?;
            }
            if let Some(id) = &metadata.id {
                visitor(SemanticNode::MetadataId(id))?;
            }
            for role in &metadata.roles {
                visitor(SemanticNode::MetadataRole(role))?;
            }
            for option in &metadata.options {
                visitor(SemanticNode::MetadataOption(option))?;
            }
            for attribute in &metadata.attributes {
                visitor(SemanticNode::ElementAttribute(attribute))?;
            }
        }
        SemanticNode::MetadataTitle(title) => {
            visit_inline_children(&title.inlines, visitor)?;
        }
        SemanticNode::Attribute(_)
        | SemanticNode::Anchor(_)
        | SemanticNode::MetadataId(_)
        | SemanticNode::MetadataRole(_)
        | SemanticNode::MetadataOption(_)
        | SemanticNode::ElementAttribute(_) => {}
    }
    ControlFlow::Continue(())
}

fn visit_block_children<'document, Break>(
    blocks: &'document [AstBlock],
    visitor: &mut impl FnMut(SemanticNode<'document>) -> ControlFlow<Break>,
) -> ControlFlow<Break> {
    for block in blocks {
        visitor(SemanticNode::Block(block))?;
    }
    ControlFlow::Continue(())
}

fn visit_inline_children<'document, Break>(
    inlines: &'document [Inline],
    visitor: &mut impl FnMut(SemanticNode<'document>) -> ControlFlow<Break>,
) -> ControlFlow<Break> {
    for inline in inlines {
        visitor(SemanticNode::Inline(inline))?;
    }
    ControlFlow::Continue(())
}

#[cfg(test)]
mod tests {
    use std::ops::ControlFlow;

    use super::{
        SemanticNode, SemanticVisitorMut, try_walk_ast, try_walk_block_slice, walk, walk_ast,
        walk_block_slice, walk_semantic_mut_cancellable,
    };
    use crate::{inline::Inline, parser::AstBlock};

    type NodeIdentity = (&'static str, usize);

    fn node_snapshot(node: SemanticNode<'_>) -> String {
        fn ranged(kind: &str, range: crate::source::TextRange, detail: &str) -> String {
            format!(
                "{kind}@{}..{}:{detail}",
                range.start().to_u32(),
                range.end().to_u32()
            )
        }

        match node {
            SemanticNode::Block(value) => {
                let kind = match value {
                    AstBlock::Heading(_) => "block-heading",
                    AstBlock::Paragraph(_) => "block-paragraph",
                    AstBlock::LiteralParagraph(_) => "block-literal-paragraph",
                    AstBlock::Break(_) => "block-break",
                    AstBlock::Source(_) => "block-source",
                    AstBlock::Verbatim(_) => "block-verbatim",
                    AstBlock::List(_) => "block-list",
                    AstBlock::Math(_) => "block-math",
                    AstBlock::Delimited(_) => "block-delimited",
                    AstBlock::Unsupported(_) => "block-unsupported",
                };
                ranged(kind, value.range(), "")
            }
            SemanticNode::List(value) => ranged("list", value.range, ""),
            SemanticNode::ListItem(value) => ranged("list-item", value.range, &value.text),
            SemanticNode::Table(value) => ranged("table", value.content_range, ""),
            SemanticNode::TableRow(value) => ranged("table-row", value.range, ""),
            SemanticNode::TableCell(value) => ranged("table-cell", value.range, &value.raw),
            SemanticNode::Inline(value) => {
                let (kind, detail) = match value {
                    Inline::Text(value) => ("inline-text", value.value.as_str()),
                    Inline::Literal { value, .. } => ("inline-literal", value.as_str()),
                    Inline::Styled { style, .. } => {
                        return ranged("inline-styled", value.range(), &format!("{style:?}"));
                    }
                    Inline::AttributeReference { name, .. } => {
                        ("inline-attribute-reference", name.as_str())
                    }
                    Inline::Link(value) => ("inline-link", value.target.as_str()),
                    Inline::Reference(value) => {
                        ("inline-reference", value.expanded_target.as_str())
                    }
                    Inline::Formula(value) => ("inline-formula", value.value.as_str()),
                    Inline::Macro(value) => ("inline-macro", value.target.as_str()),
                    Inline::Passthrough { value, .. } => ("inline-passthrough", value.as_str()),
                    Inline::HardBreak { .. } => ("inline-hard-break", ""),
                };
                ranged(kind, value.range(), detail)
            }
            SemanticNode::Attribute(value) => ranged("attribute", value.range, &value.name),
            SemanticNode::Anchor(value) => ranged("anchor", value.range, &value.id),
            SemanticNode::Metadata(value) => value.range.map_or_else(
                || "metadata@none:".to_owned(),
                |range| ranged("metadata", range, ""),
            ),
            SemanticNode::MetadataTitle(value) => {
                ranged("metadata-title", value.range, &value.value)
            }
            SemanticNode::MetadataId(value) => ranged("metadata-id", value.range, &value.value),
            SemanticNode::MetadataRole(value) => ranged("metadata-role", value.range, &value.value),
            SemanticNode::MetadataOption(value) => {
                ranged("metadata-option", value.range, &value.value)
            }
            SemanticNode::ElementAttribute(value) => ranged(
                "element-attribute",
                value.range,
                &format!("{}={}", value.name.as_deref().unwrap_or(""), value.value),
            ),
        }
    }

    fn node_identity(node: SemanticNode<'_>) -> NodeIdentity {
        fn address<T>(value: &T) -> usize {
            value as *const T as usize
        }

        match node {
            SemanticNode::Block(value) => ("block", address(value)),
            SemanticNode::List(value) => ("list", address(value)),
            SemanticNode::ListItem(value) => ("list-item", address(value)),
            SemanticNode::Table(value) => ("table", address(value)),
            SemanticNode::TableRow(value) => ("table-row", address(value)),
            SemanticNode::TableCell(value) => ("table-cell", address(value)),
            SemanticNode::Inline(value) => ("inline", address(value)),
            SemanticNode::Attribute(value) => ("attribute", address(value)),
            SemanticNode::Anchor(value) => ("anchor", address(value)),
            SemanticNode::Metadata(value) => ("metadata", address(value)),
            SemanticNode::MetadataTitle(value) => ("metadata-title", address(value)),
            SemanticNode::MetadataId(value) => ("metadata-id", address(value)),
            SemanticNode::MetadataRole(value) => ("metadata-role", address(value)),
            SemanticNode::MetadataOption(value) => ("metadata-option", address(value)),
            SemanticNode::ElementAttribute(value) => ("element-attribute", address(value)),
        }
    }

    fn controlled_walk_fixture() -> crate::parser::ParsedDocument {
        crate::parser::parse(concat!(
            ":name: value\n",
            "\n",
            "[[document-anchor]]\n",
            ".Block title\n",
            "[#block-id.role%option,key=value]\n",
            "paragraph https://example.test[*link*] xref:document.adoc[*reference*]\n",
            "\n",
            "term:: *description*\n",
            "\n",
            "* outer\n",
            "** nested\n",
            "+\n",
            "continuation\n",
            "\n",
            "====\n",
            "compound\n",
            "====\n",
            "\n",
            "[cols=\"1,1a\"]\n",
            "|===\n",
            "|inline cell\n",
            "|AsciiDoc *cell*\n",
            "|===\n",
            "\n",
            "after\n",
        ))
        .expect("controlled walker fixture")
    }

    #[test]
    fn controlled_walk_continue_matches_legacy_walk_exactly() {
        let parsed = controlled_walk_fixture();
        let mut legacy = Vec::new();
        walk_ast(&parsed.ast, |node| legacy.push(node_identity(node)));
        let mut controlled = Vec::new();
        let mut snapshots = Vec::new();

        let result = try_walk_ast(&parsed.ast, |node| {
            controlled.push(node_identity(node));
            snapshots.push(node_snapshot(node));
            ControlFlow::<usize>::Continue(())
        });

        assert_eq!(result, ControlFlow::Continue(()));
        assert_eq!(controlled, legacy);
        assert_eq!(
            controlled
                .iter()
                .copied()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            controlled.len(),
            "each semantic node must be emitted exactly once"
        );
        assert_eq!(
            snapshots,
            [
                "attribute@0..13:name",
                "anchor@14..34:document-anchor",
                "anchor@47..81:block-id",
                "block-paragraph@81..152:",
                "metadata@14..81:",
                "metadata-title@35..46:Block title",
                "inline-text@35..46:Block title",
                "metadata-id@49..57:block-id",
                "metadata-role@58..62:role",
                "metadata-option@63..69:option",
                "element-attribute@70..79:key=value",
                "inline-text@81..91:paragraph ",
                "inline-link@91..119:https://example.test",
                "inline-styled@112..118:Strong",
                "inline-text@113..117:link",
                "inline-text@119..120: ",
                "inline-reference@120..151:document.adoc",
                "inline-styled@139..150:Strong",
                "inline-text@140..149:reference",
                "block-list@153..174:",
                "metadata@none:",
                "list@153..174:",
                "list-item@153..174:*description*",
                "inline-text@153..157:term",
                "inline-styled@160..173:Strong",
                "inline-text@161..172:description",
                "block-list@175..208:",
                "metadata@none:",
                "list@175..208:",
                "list-item@175..208:outer",
                "inline-text@177..182:outer",
                "list@183..208:",
                "list-item@183..208:nested",
                "inline-text@186..192:nested",
                "block-paragraph@195..208:",
                "metadata@none:",
                "inline-text@195..207:continuation",
                "block-delimited@209..228:",
                "metadata@none:",
                "block-paragraph@214..223:",
                "metadata@none:",
                "inline-text@214..222:compound",
                "block-delimited@243..283:",
                "metadata@229..243:",
                "element-attribute@230..241:cols=1,1a",
                "table@248..278:",
                "table-row@248..277:",
                "table-cell@248..260:inline cell",
                "inline-text@249..260:inline cell",
                "table-cell@261..277:AsciiDoc *cell*",
                "block-paragraph@262..277:",
                "metadata@none:",
                "inline-text@262..271:AsciiDoc ",
                "inline-styled@271..277:Strong",
                "inline-text@272..276:cell",
                "block-paragraph@284..290:",
                "metadata@none:",
                "inline-text@284..289:after",
            ]
        );
        for expected in [
            "attribute",
            "anchor",
            "block",
            "metadata",
            "metadata-title",
            "metadata-id",
            "metadata-role",
            "metadata-option",
            "element-attribute",
            "list",
            "list-item",
            "table",
            "table-row",
            "table-cell",
            "inline",
        ] {
            assert!(
                controlled.iter().any(|(kind, _)| *kind == expected),
                "fixture did not exercise {expected}"
            );
        }
    }

    #[test]
    fn controlled_walk_breaks_at_every_visited_prefix() {
        let parsed = controlled_walk_fixture();
        let mut complete = Vec::new();
        walk_ast(&parsed.ast, |node| complete.push(node_identity(node)));
        assert!(!complete.is_empty());

        for break_at in 0..complete.len() {
            let mut visited = Vec::new();
            let result = try_walk_ast(&parsed.ast, |node| {
                visited.push(node_identity(node));
                if visited.len() - 1 == break_at {
                    ControlFlow::Break(break_at)
                } else {
                    ControlFlow::Continue(())
                }
            });

            assert_eq!(result, ControlFlow::Break(break_at));
            assert_eq!(visited, complete[..=break_at]);
        }
    }

    #[test]
    fn controlled_block_slice_preserves_scope_and_nested_break_payload() {
        let parsed = controlled_walk_fixture();
        let mut legacy = Vec::new();
        walk_block_slice(&parsed.ast.blocks, |node| {
            legacy.push(node_identity(node));
        });
        assert!(
            legacy
                .iter()
                .all(|(kind, _)| !matches!(*kind, "attribute" | "anchor"))
        );

        let mut controlled = Vec::new();
        let result = try_walk_block_slice(&parsed.ast.blocks, |node| {
            controlled.push(node_identity(node));
            if matches!(
                node,
                SemanticNode::Inline(crate::inline::Inline::Text(text))
                    if text.value.contains("AsciiDoc")
            ) {
                ControlFlow::Break("asciidoc-cell")
            } else {
                ControlFlow::Continue(())
            }
        });

        assert_eq!(result, ControlFlow::Break("asciidoc-cell"));
        assert_eq!(controlled, legacy[..controlled.len()]);
        assert!(controlled.len() < legacy.len());
    }

    #[test]
    fn controlled_walk_empty_document_continues_without_callbacks() {
        let parsed = crate::parser::parse("").expect("empty source");
        let callbacks = std::cell::Cell::new(0);

        let result = try_walk_ast(&parsed.ast, |_| {
            callbacks.set(callbacks.get() + 1);
            ControlFlow::<()>::Continue(())
        });

        assert_eq!(result, ControlFlow::Continue(()));
        assert_eq!(callbacks.get(), 0);
    }

    #[test]
    fn walk_preserves_rows_and_cells_from_empty_column_specs() {
        let parsed = crate::parser::parse(
            "[cols=\",,\",options=header]\n|===\n|ID |検証 |受入\n|REQ-001 |自動試験 |手動確認\n|===\n",
        )
        .expect("table");
        let mut tables = 0;
        let mut rows = 0;
        let mut cells = Vec::new();
        walk_ast(&parsed.ast, |node| match node {
            SemanticNode::Table(table) => {
                tables += 1;
                assert_eq!(table.columns.len(), 3);
            }
            SemanticNode::TableRow(_) => rows += 1,
            SemanticNode::TableCell(cell) => cells.push(cell.raw.as_str()),
            _ => {}
        });
        assert_eq!(tables, 1);
        assert_eq!(rows, 2);
        assert_eq!(
            cells,
            ["ID", "検証", "受入", "REQ-001", "自動試験", "手動確認"]
        );
    }

    #[test]
    fn walk_visits_nested_lists_continuations_and_inline_labels_once() {
        let analysis = crate::Engine::new(crate::AnalysisOptions::default())
            .analyze("* outer\n** https://example.test[*label*]\n+\n....\nbody\n....\n")
            .expect("source");
        let mut blocks = 0;
        let mut lists = 0;
        let mut items = 0;
        let mut inlines = 0;
        walk(analysis.document(), |node| match node {
            SemanticNode::Block(_) => blocks += 1,
            SemanticNode::List(_) => lists += 1,
            SemanticNode::ListItem(_) => items += 1,
            SemanticNode::Table(_) | SemanticNode::TableRow(_) | SemanticNode::TableCell(_) => {}
            SemanticNode::Inline(_) => inlines += 1,
            SemanticNode::Attribute(_)
            | SemanticNode::Anchor(_)
            | SemanticNode::Metadata(_)
            | SemanticNode::MetadataTitle(_)
            | SemanticNode::MetadataId(_)
            | SemanticNode::MetadataRole(_)
            | SemanticNode::MetadataOption(_)
            | SemanticNode::ElementAttribute(_) => {}
        });
        assert_eq!(blocks, 2);
        assert_eq!(lists, 2);
        assert_eq!(items, 2);
        assert!(inlines >= 3);
    }

    #[test]
    fn every_semantic_query_observes_the_same_nested_reachability() {
        let source = concat!(
            "====\n",
            "xref:top[]\n",
            "\n",
            "* image:outer.png[]\n",
            "+\n",
            "[cols=\"a\"]\n",
            "|===\n",
            "|xref:cell[] image:cell.png[]\n",
            "|===\n",
            "====\n",
        );
        let analysis = crate::Engine::new(crate::AnalysisOptions::default())
            .analyze(source)
            .expect("source");
        let mut walked_references = 0;
        let mut walked_macros = 0;
        walk(analysis.document(), |node| {
            if let SemanticNode::Inline(inline) = node {
                match inline {
                    crate::inline::Inline::Reference(_) => walked_references += 1,
                    crate::inline::Inline::Macro(_) => walked_macros += 1,
                    _ => {}
                }
            }
        });

        assert_eq!(analysis.references().len(), walked_references);
        assert_eq!(analysis.macros().len(), walked_macros);
        assert_eq!(analysis.resources().len(), walked_macros);
        assert_eq!(walked_references, 2);
        assert_eq!(walked_macros, 2);
    }

    #[test]
    fn immutable_and_mutable_walkers_reach_the_same_topology() {
        let source = concat!(
            "====\n",
            "* outer\n",
            "** nested\n",
            "+\n",
            "....\n",
            "literal\n",
            "....\n",
            "\n",
            "[cols=a]\n",
            "|===\n",
            "|== Cell\n",
            "|===\n",
            "====\n",
        );
        let mut parsed = crate::parser::parse(source).expect("nested source");
        let mut immutable = Vec::new();
        walk_ast(&parsed.ast, |node| match node {
            SemanticNode::Block(block) => immutable.push(("block", block.range())),
            SemanticNode::List(list) => immutable.push(("list", list.range)),
            SemanticNode::Inline(inline) => immutable.push(("inline", inline.range())),
            _ => {}
        });

        #[derive(Default)]
        struct Events(Vec<(&'static str, crate::source::TextRange)>);
        impl SemanticVisitorMut for Events {
            fn visit_block(&mut self, block: &mut AstBlock) {
                self.0.push(("block", block.range()));
            }

            fn visit_list(&mut self, list: &mut crate::parser::ListBlock) {
                self.0.push(("list", list.range));
            }

            fn visit_inline_sequence(&mut self, inlines: &mut Vec<Inline>) {
                fn collect(
                    inlines: &[Inline],
                    output: &mut Vec<(&'static str, crate::source::TextRange)>,
                ) {
                    for inline in inlines {
                        output.push(("inline", inline.range()));
                        match inline {
                            Inline::Styled { children, .. } => collect(children, output),
                            Inline::Link(link) => collect(&link.label, output),
                            Inline::Reference(reference) => collect(&reference.label, output),
                            Inline::Macro(_)
                            | Inline::Text(_)
                            | Inline::Literal { .. }
                            | Inline::AttributeReference { .. }
                            | Inline::HardBreak { .. }
                            | Inline::Passthrough { .. }
                            | Inline::Formula(_) => {}
                        }
                    }
                }
                collect(inlines, &mut self.0);
            }
        }
        let mut mutable = Events::default();
        walk_semantic_mut_cancellable(
            &mut parsed.ast.blocks,
            &mut mutable,
            &mut crate::cancellation::CancellationCheckpoint::new(&crate::core::NeverCancel),
        )
        .expect("NeverCancel cannot cancel mutable walking");

        assert_eq!(immutable, mutable.0);
    }

    #[test]
    fn failed_budget_or_cancellation_produces_no_walkable_semantic_tree() {
        let mut limited = crate::AnalysisOptions::default();
        limited.syntax.limits.max_blocks = 1;
        let budget_result = crate::Engine::new(limited).analyze("one\n\ntwo\n");
        assert!(matches!(
            budget_result,
            Err(crate::ParseError::LimitExceeded {
                resource: "blocks",
                ..
            })
        ));

        let cancellation = crate::CancellationToken::new();
        cancellation.cancel();
        let cancellation_result = crate::Engine::new(crate::AnalysisOptions::default())
            .analyze_with(
                "paragraph\n",
                crate::AnalysisInputs {
                    cancellation: Some(&cancellation),
                    ..crate::AnalysisInputs::default()
                },
            );
        assert!(matches!(
            cancellation_result,
            Err(crate::ParseError::Cancelled)
        ));
    }

    #[test]
    fn final_semantic_tree_contains_no_parser_recovery_state() {
        for source in [
            "==Missing\n",
            "paragraph **open\n",
            "[source]\n----\n== Next\n",
            "[cols=a]\n|===\n|[source,rust]\n----\nfn main() {}\n----\n|===\n",
            "*  item\n",
            "[stem]\n++++\nopen\n== Next\n",
        ] {
            let analysis = crate::Engine::new(crate::AnalysisOptions::default())
                .analyze(source)
                .expect("recoverable source");
            walk(analysis.document(), |node| match node {
                SemanticNode::Block(block) => match block {
                    AstBlock::Heading(value) => {
                        assert!(value.problems.is_empty());
                        assert!(value.inline_problems.is_empty());
                    }
                    AstBlock::Paragraph(value) => assert!(value.inline_problems.is_empty()),
                    AstBlock::Source(_) => {
                        panic!("parser-only source blocks must not reach the semantic document")
                    }
                    AstBlock::Verbatim(value) => assert!(value.problems.is_empty()),
                    AstBlock::Math(value) => assert!(value.problems.is_empty()),
                    AstBlock::Delimited(value) => assert!(value.problems.is_empty()),
                    AstBlock::List(_)
                    | AstBlock::LiteralParagraph(_)
                    | AstBlock::Break(_)
                    | AstBlock::Unsupported(_) => {}
                },
                SemanticNode::ListItem(item) => {
                    assert!(item.problems.is_empty());
                    assert!(item.inline_problems.is_empty());
                    assert!(
                        item.terms
                            .iter()
                            .all(|term| term.inline_problems.is_empty())
                    );
                }
                SemanticNode::List(_)
                | SemanticNode::Table(_)
                | SemanticNode::TableRow(_)
                | SemanticNode::TableCell(_)
                | SemanticNode::Inline(_)
                | SemanticNode::Attribute(_)
                | SemanticNode::Anchor(_)
                | SemanticNode::Metadata(_)
                | SemanticNode::MetadataTitle(_)
                | SemanticNode::MetadataId(_)
                | SemanticNode::MetadataRole(_)
                | SemanticNode::MetadataOption(_)
                | SemanticNode::ElementAttribute(_) => {}
            });
        }
    }
}
