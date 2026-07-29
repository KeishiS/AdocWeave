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

pub(crate) fn walk_blocks_mut(blocks: &mut [AstBlock], visitor: &mut impl BlockVisitorMut) {
    fn walk_list_mut(list: &mut ListBlock, visitor: &mut impl BlockVisitorMut) {
        visitor.visit_list(list);
        for item in &mut list.items {
            for child in &mut item.children {
                walk_list_mut(child, visitor);
            }
            walk_blocks_mut(&mut item.continuations, visitor);
        }
    }

    for block in blocks {
        visitor.visit_block(block);
        match block {
            AstBlock::List(list) => walk_list_mut(list, visitor),
            AstBlock::Delimited(block) => match &mut block.content {
                crate::parser::DelimitedContent::Compound(children) => {
                    walk_blocks_mut(children, visitor);
                }
                crate::parser::DelimitedContent::Table(table) => {
                    for row in &mut table.rows {
                        for cell in &mut row.cells {
                            if let crate::table::TableCellContent::AsciiDoc(children) =
                                &mut cell.content
                            {
                                walk_blocks_mut(children, visitor);
                            }
                        }
                    }
                }
                crate::parser::DelimitedContent::Verbatim(_)
                | crate::parser::DelimitedContent::Passthrough(_) => {}
            },
            AstBlock::Heading(_)
            | AstBlock::Paragraph(_)
            | AstBlock::LiteralParagraph(_)
            | AstBlock::Break(_)
            | AstBlock::Source(_)
            | AstBlock::Verbatim(_)
            | AstBlock::Math(_)
            | AstBlock::Unsupported(_) => {}
        }
    }
}

pub(crate) fn walk_inline_sequences_mut(
    blocks: &mut [AstBlock],
    visitor: &mut impl FnMut(&mut Vec<Inline>),
) {
    fn visit_list(list: &mut ListBlock, visitor: &mut impl FnMut(&mut Vec<Inline>)) {
        for item in &mut list.items {
            for term in &mut item.terms {
                visitor(&mut term.inlines);
            }
            visitor(&mut item.inlines);
            for child in &mut item.children {
                visit_list(child, visitor);
            }
            walk_inline_sequences_mut(&mut item.continuations, visitor);
        }
    }

    for block in blocks {
        if let Some(title) = &mut block.metadata_mut().title {
            visitor(&mut title.inlines);
        }
        match block {
            AstBlock::Heading(heading) => visitor(&mut heading.inlines),
            AstBlock::Paragraph(paragraph) => visitor(&mut paragraph.inlines),
            AstBlock::List(list) => visit_list(list, visitor),
            AstBlock::Delimited(block) => match &mut block.content {
                crate::parser::DelimitedContent::Compound(children) => {
                    walk_inline_sequences_mut(children, visitor);
                }
                crate::parser::DelimitedContent::Table(table) => {
                    for row in &mut table.rows {
                        for cell in &mut row.cells {
                            match &mut cell.content {
                                crate::table::TableCellContent::Inlines(inlines) => {
                                    visitor(inlines)
                                }
                                crate::table::TableCellContent::AsciiDoc(children) => {
                                    walk_inline_sequences_mut(children, visitor);
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
}

fn try_walk_blocks<'document, Break>(
    blocks: &'document [AstBlock],
    visitor: &mut impl FnMut(SemanticNode<'document>) -> ControlFlow<Break>,
) -> ControlFlow<Break> {
    for block in blocks {
        visitor(SemanticNode::Block(block))?;
        try_walk_metadata(block.metadata(), visitor)?;
        match block {
            AstBlock::Heading(heading) => try_walk_inlines(&heading.inlines, visitor)?,
            AstBlock::Paragraph(paragraph) => try_walk_inlines(&paragraph.inlines, visitor)?,
            AstBlock::List(list) => {
                visitor(SemanticNode::List(list))?;
                try_walk_list_contents(list, visitor)?;
            }
            AstBlock::Delimited(block) => match &block.content {
                crate::parser::DelimitedContent::Compound(children) => {
                    try_walk_blocks(children, visitor)?;
                }
                crate::parser::DelimitedContent::Table(table) => {
                    visitor(SemanticNode::Table(table))?;
                    for row in &table.rows {
                        visitor(SemanticNode::TableRow(row))?;
                        for cell in &row.cells {
                            visitor(SemanticNode::TableCell(cell))?;
                            match &cell.content {
                                crate::table::TableCellContent::Inlines(inlines) => {
                                    try_walk_inlines(inlines, visitor)?;
                                }
                                crate::table::TableCellContent::AsciiDoc(blocks) => {
                                    try_walk_blocks(blocks, visitor)?;
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
    ControlFlow::Continue(())
}

fn try_walk_metadata<'document, Break>(
    metadata: &'document BlockMetadata,
    visitor: &mut impl FnMut(SemanticNode<'document>) -> ControlFlow<Break>,
) -> ControlFlow<Break> {
    visitor(SemanticNode::Metadata(metadata))?;
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
    ControlFlow::Continue(())
}

fn try_walk_list_contents<'document, Break>(
    list: &'document ListBlock,
    visitor: &mut impl FnMut(SemanticNode<'document>) -> ControlFlow<Break>,
) -> ControlFlow<Break> {
    for item in &list.items {
        visitor(SemanticNode::ListItem(item))?;
        for term in &item.terms {
            try_walk_inlines(&term.inlines, visitor)?;
        }
        try_walk_inlines(&item.inlines, visitor)?;
        for child in &item.children {
            visitor(SemanticNode::List(child))?;
            try_walk_list_contents(child, visitor)?;
        }
        try_walk_blocks(&item.continuations, visitor)?;
    }
    ControlFlow::Continue(())
}

fn try_walk_inlines<'document, Break>(
    inlines: &'document [Inline],
    visitor: &mut impl FnMut(SemanticNode<'document>) -> ControlFlow<Break>,
) -> ControlFlow<Break> {
    for inline in inlines {
        visitor(SemanticNode::Inline(inline))?;
        match inline {
            Inline::Styled { children, .. } => try_walk_inlines(children, visitor)?,
            Inline::Link(link) => try_walk_inlines(&link.label, visitor)?,
            Inline::Reference(reference) => try_walk_inlines(&reference.label, visitor)?,
            Inline::Macro(_) => {}
            Inline::Text(_)
            | Inline::Literal { .. }
            | Inline::AttributeReference { .. }
            | Inline::HardBreak { .. }
            | Inline::Passthrough { .. }
            | Inline::Formula(_) => {}
        }
    }
    ControlFlow::Continue(())
}

#[cfg(test)]
mod tests {
    use std::ops::ControlFlow;

    use super::{
        BlockVisitorMut, SemanticNode, try_walk_ast, try_walk_block_slice, walk, walk_ast,
        walk_block_slice, walk_blocks_mut,
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
            snapshots,
            [
                "attribute@0..13:name",
                "anchor@14..34:document-anchor",
                "anchor@47..81:block-id",
                "block-paragraph@81..152:",
                "metadata@14..81:",
                "metadata-title@35..46:Block title",
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
    fn immutable_and_mutable_walkers_reach_the_same_blocks_and_lists() {
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
        let mut immutable = (0, 0);
        walk_ast(&parsed.ast, |node| match node {
            SemanticNode::Block(_) => immutable.0 += 1,
            SemanticNode::List(_) => immutable.1 += 1,
            _ => {}
        });

        #[derive(Default)]
        struct Counts(usize, usize);
        impl BlockVisitorMut for Counts {
            fn visit_block(&mut self, _block: &mut AstBlock) {
                self.0 += 1;
            }

            fn visit_list(&mut self, _list: &mut crate::parser::ListBlock) {
                self.1 += 1;
            }
        }
        let mut mutable = Counts::default();
        walk_blocks_mut(&mut parsed.ast.blocks, &mut mutable);

        assert_eq!(immutable, (mutable.0, mutable.1));
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
