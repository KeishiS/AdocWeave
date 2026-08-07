use std::sync::atomic::{AtomicUsize, Ordering};

use super::{
    AdmonitionKind, AstBlock, BreakBlock, BreakKind, ChecklistState, DelimitedBlockKind,
    DelimitedContent, DocumentType, Heading, HeadingKind, ListKind, SourceInfo, SyntaxKind,
    VerbatimBlock, VerbatimKind, parse,
};

#[test]
fn finish_document_propagates_lowering_cancellation_without_partial_output() {
    struct CancelAfterFirstCheckpoint(AtomicUsize);

    impl crate::core::CancellationCheck for CancelAfterFirstCheckpoint {
        fn is_cancelled(&self) -> bool {
            self.0.fetch_add(1, Ordering::Relaxed) >= 1
        }
    }

    let source = (0..crate::cancellation::CHECKPOINT_INTERVAL * 2)
        .map(|index| format!("paragraph {index}\n\n"))
        .collect::<String>();
    let source: std::sync::Arc<str> = std::sync::Arc::from(source);
    let config = super::ParseConfig::default();
    let source_document =
        crate::source::SourceDocument::from_shared(std::sync::Arc::clone(&source))
            .expect("source document");
    let line_count = source_document.lines().len();
    let mut budget = super::ParseBudget::new(config.limits).expect("default limits");
    let sequence = super::parse_block_sequence(
        source.as_ref(),
        super::BlockInput::new(&source_document, 0..line_count).expect("block input"),
        &config,
        &|| false,
        &mut budget,
        super::BlockContext::root(),
    )
    .expect("block parsing completes");
    let cancellation = CancelAfterFirstCheckpoint(AtomicUsize::new(0));

    let result = super::finish_document(
        sequence,
        source_document,
        &config,
        &std::collections::BTreeMap::new(),
        &cancellation,
    );

    assert!(matches!(
        result,
        Err(crate::parser_support::ParseFailure::Cancelled)
    ));
    assert_eq!(cancellation.0.load(Ordering::Relaxed), 2);
}

#[test]
fn valid_anchor_id_rejects_html_attribute_metacharacters() {
    assert!(crate::block_grammar::valid_anchor_id("section-1"));
    assert!(crate::block_grammar::valid_anchor_id("item.lead"));
    for id in ["a\"b", "a'b", "a&b", "a=b", "a(b)", "a b", "a\tb", ""] {
        assert!(
            !crate::block_grammar::valid_anchor_id(id),
            "expected {id:?} to be invalid"
        );
    }
}

#[test]
fn block_cursor_rejects_non_progress_and_out_of_bounds_commits() {
    let mut cursor = super::BlockCursor::new(2);
    assert_eq!(cursor.current(), Some(0));
    assert!(cursor.commit(super::BlockConsumption::Through(0)).is_err());
    assert!(cursor.commit(super::BlockConsumption::Through(3)).is_err());
    cursor
        .commit(super::BlockConsumption::OneLine)
        .expect("first line");
    cursor
        .commit(super::BlockConsumption::Through(2))
        .expect("second line");
    assert_eq!(cursor.current(), None);
}

#[test]
fn recognized_block_commit_is_atomic_for_cursor_budget_syntax_and_facts() {
    fn recognized(
        consumption: super::BlockConsumption,
    ) -> super::BlockRecognition<super::BlockCommit> {
        let range = crate::source::TextRange::new(
            crate::source::TextSize::ZERO,
            crate::source::TextSize::new(1).expect("test offset"),
        )
        .expect("test range");
        super::BlockRecognition::recovered(
            consumption,
            super::BlockCommit::single(
                crate::syntax::SyntaxNode::leaf(SyntaxKind::PageBreak, range),
                AstBlock::Break(BreakBlock {
                    metadata: Default::default(),
                    range,
                    kind: BreakKind::Page,
                }),
                0,
            ),
        )
    }

    let mut parser = super::BlockParserState {
        cursor: super::BlockCursor::new(2),
        document_header_phase: Some(super::DocumentHeaderState::default()),
        pending_metadata: super::PendingBlockMetadata::default(),
        paragraph_lines: Vec::new(),
        saw_content: false,
        context: super::BlockContext::root(),
    };
    let mut syntax = Vec::new();
    let mut blocks = Vec::new();
    let mut budget = crate::budget::ParseBudget::new(crate::AnalysisLimits {
        max_blocks: 1,
        max_nodes: 2,
        ..crate::AnalysisLimits::default()
    })
    .expect("document node budget");

    assert!(
        !super::commit_recognized_block(
            &mut parser,
            super::BlockRecognition::NoMatch,
            &mut syntax,
            &mut blocks,
            &mut budget,
        )
        .expect("no match")
    );
    assert!(
        super::commit_recognized_block(
            &mut parser,
            recognized(super::BlockConsumption::Through(0)),
            &mut syntax,
            &mut blocks,
            &mut budget,
        )
        .is_err()
    );
    assert_eq!(parser.cursor.current(), Some(0));
    assert!(syntax.is_empty());
    assert!(blocks.is_empty());
    assert!(parser.pending_metadata.is_empty());

    assert!(
        super::commit_recognized_block(
            &mut parser,
            recognized(super::BlockConsumption::OneLine),
            &mut syntax,
            &mut blocks,
            &mut budget,
        )
        .expect("valid recovered block")
    );
    assert_eq!(parser.cursor.current(), Some(1));
    assert_eq!(syntax.len(), 1);
    assert_eq!(blocks.len(), 1);

    assert!(matches!(
        super::commit_recognized_block(
            &mut parser,
            recognized(super::BlockConsumption::OneLine),
            &mut syntax,
            &mut blocks,
            &mut budget,
        ),
        Err(super::ParseFailure::Budget(_))
    ));
    assert_eq!(parser.cursor.current(), Some(1));
    assert_eq!(syntax.len(), 1);
    assert_eq!(blocks.len(), 1);
}

#[test]
fn parser_state_flushes_paragraphs_and_attaches_metadata_once_at_each_depth() {
    fn assert_paragraphs(blocks: &[AstBlock]) {
        let [AstBlock::Paragraph(first), AstBlock::Paragraph(second)] = blocks else {
            panic!("expected two paragraphs");
        };
        assert_eq!(first.value, "first\ncontinued");
        assert_eq!(
            first.metadata.id.as_ref().map(|id| id.value.as_str()),
            Some("only")
        );
        assert!(
            second.metadata.id.is_none(),
            "pending metadata must be consumed by the adjacent block"
        );
    }

    let root = parse("[#only]\nfirst\ncontinued\n\nsecond\n").expect("root paragraphs");
    assert_paragraphs(root.ast.blocks());

    let nested = parse("--\n[#only]\nfirst\ncontinued\n\nsecond\n--\n").expect("nested paragraphs");
    let [AstBlock::Delimited(block)] = nested.ast.blocks() else {
        panic!("expected open block");
    };
    let super::DelimitedContent::Compound(blocks) = &block.content else {
        panic!("expected compound content");
    };
    assert_paragraphs(blocks);
}

#[test]
fn parser_state_closes_the_document_header_before_body_attributes() {
    let parsed = parse("= Title\n\nparagraph\n\n:body-name: value\n").expect("root document");

    assert!(parsed.ast.header_attributes().is_empty());
    assert_eq!(parsed.ast.attributes().len(), 1);
    assert_eq!(parsed.ast.attributes()[0].name, "body-name");
    assert!(matches!(
        parsed.ast.blocks(),
        [AstBlock::Heading(_), AstBlock::Paragraph(_)]
    ));
}

#[test]
fn orphan_metadata_and_following_comment_reconstruct_in_source_order() {
    for source in ["[]\n//", "[é]\n//", "[\0]\n//\0n\n\n"] {
        let parsed = parse(source).expect("recoverable metadata and comment");
        assert_eq!(parsed.syntax.reconstruct(), source, "{source:?}");
        let ranges = parsed
            .syntax
            .root()
            .descendants()
            .filter_map(|node| match node.kind() {
                SyntaxKind::Token(_) => Some(node.range()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let mut cursor = crate::source::TextSize::ZERO;
        for range in ranges {
            assert_eq!(range.start(), cursor, "{source:?}");
            assert!(range.start() < range.end(), "{source:?}");
            assert!(source.is_char_boundary(range.start().to_usize()));
            assert!(source.is_char_boundary(range.end().to_usize()));
            cursor = range.end();
        }
        assert_eq!(cursor.to_usize(), source.len(), "{source:?}");

        let AstBlock::Unsupported(unsupported) = &parsed.ast.blocks()[0] else {
            panic!("orphan metadata is represented as unsupported");
        };
        assert_eq!(
            unsupported.range.end().to_usize(),
            source.find("//").unwrap()
        );
        assert!(!unsupported.raw.contains("//"));
        assert_eq!(
            parsed.syntax.blocks()[1].kind(),
            SyntaxKind::CommentLine,
            "a trailing comment remains an independent root block"
        );
    }
}

#[test]
fn unclosed_nested_delimiter_stops_at_the_parent_boundary() {
    let source = "=e\n--\n----\n--\nfmfm";
    let parsed = parse(source).expect("recoverable nested delimiter");

    assert_eq!(parsed.syntax.reconstruct(), source);
    let ranges = parsed
        .syntax
        .blocks()
        .iter()
        .map(|block| {
            (
                block.range().start().to_usize(),
                block.range().end().to_usize(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(ranges, vec![(0, 3), (3, 14), (14, 18)]);

    let AstBlock::Delimited(outer) = &parsed.ast.blocks()[1] else {
        panic!("outer open block");
    };
    let DelimitedContent::Compound(children) = &outer.content else {
        panic!("outer open block has compound content");
    };
    let AstBlock::Verbatim(inner) = &children[0] else {
        panic!("inner listing block");
    };
    assert_eq!(inner.range.start().to_usize(), 6);
    assert_eq!(inner.range.end().to_usize(), 11);
    assert!(parsed.syntax.issues().iter().any(|issue| {
        issue.class == crate::syntax::SyntaxIssueClass::UnclosedBlock
            && issue.range == inner.delimiter_range
    }));
}

#[test]
fn attributed_nested_blocks_stop_at_the_parent_boundary() {
    for source in [
        "--\n[source]\n----\n--",
        "--\n[stem]\n++++\n--",
        "--\n* item\n+\n[source]\n----\n--",
    ] {
        let parsed = parse(source).expect("recoverable attributed nested block");
        assert_eq!(parsed.syntax.reconstruct(), source, "{source:?}");
        assert_eq!(
            parsed
                .syntax
                .blocks()
                .last()
                .map(|block| block.range().end()),
            Some(crate::source::TextSize::new(source.len()).expect("bounded source")),
            "{source:?}"
        );
    }
}

#[test]
fn comments_between_metadata_and_a_block_remain_attached_syntax() {
    let source = "[#identifier]\n// interstitial comment\nparagraph\n";
    let parsed = parse(source).expect("metadata, comment, and paragraph");

    assert_eq!(parsed.syntax.reconstruct(), source);
    assert_eq!(
        parsed.syntax.nodes(SyntaxKind::CommentLine).count(),
        1,
        "the interstitial comment remains queryable"
    );
    let AstBlock::Paragraph(paragraph) = &parsed.ast.blocks()[0] else {
        panic!("paragraph");
    };
    assert_eq!(
        paragraph.metadata.id.as_ref().map(|id| id.value.as_str()),
        Some("identifier"),
        "comments do not detach metadata from the following block"
    );
}

#[test]
fn orphan_metadata_separates_only_comments_after_the_last_metadata_line() {
    let source = "[role]\n// between metadata\n.Title\n// trailing\n\n";
    let parsed = parse(source).expect("orphan metadata with comments");

    assert_eq!(parsed.syntax.reconstruct(), source);
    assert_eq!(parsed.syntax.nodes(SyntaxKind::CommentLine).count(), 2);
    let AstBlock::Unsupported(unsupported) = &parsed.ast.blocks()[0] else {
        panic!("orphan metadata is represented as unsupported");
    };
    assert!(
        unsupported.raw.contains("// between metadata"),
        "an interstitial comment remains inside the recovered metadata region"
    );
    assert!(!unsupported.raw.contains("// trailing"));
    assert_eq!(parsed.syntax.blocks()[1].kind(), SyntaxKind::CommentLine);
    assert_eq!(parsed.syntax.blocks()[2].kind(), SyntaxKind::BlankLine);
}

#[test]
fn document_attribute_flushes_preceding_orphan_metadata_and_comment() {
    let source = "[#identifier]\n// trailing metadata comment\n:name: value\n\n";
    let parsed = parse(source).expect("orphan metadata before document attribute");

    assert_eq!(parsed.syntax.reconstruct(), source);
    assert_eq!(
        parsed
            .syntax
            .blocks()
            .iter()
            .map(|node| node.kind())
            .collect::<Vec<_>>(),
        [
            SyntaxKind::Unsupported,
            SyntaxKind::CommentLine,
            SyntaxKind::DocumentAttribute,
            SyntaxKind::BlankLine,
            SyntaxKind::BlankLine,
        ]
    );
    let AstBlock::Unsupported(unsupported) = &parsed.ast.blocks()[0] else {
        panic!("orphan metadata is represented as unsupported");
    };
    assert_eq!(unsupported.raw, "[#identifier]");
    assert_eq!(parsed.ast.blocks().len(), 1);
}

#[test]
fn nested_compound_blocks_share_the_root_source_index() {
    crate::source::SourceDocument::reset_construction_count();
    let source = "====\n.Outer\n--\n.Sidebar\n****\nparagraph\n****\n--\n====\n";

    let parsed = parse(source).expect("nested compound blocks");

    assert_eq!(
        crate::source::SourceDocument::construction_count(),
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
use crate::attributes::DocumentAttributeOperation;
use crate::inline_model::{Inline, MathLanguage};

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
    assert_eq!(
        parsed.ast.attributes[0].operation,
        DocumentAttributeOperation::Set
    );
    assert_eq!(
        parsed.ast.attributes[0].value.source_text,
        "123E4567-E89B-12D3-A456-426614174000"
    );
    assert_eq!(parsed.ast.attributes[2].value.source_text, "rust, AsciiDoc");
    assert_eq!(parsed.ast.attributes[3].value.source_text, "latexmath");
    assert_eq!(
        parsed.ast.attributes[4].operation,
        DocumentAttributeOperation::Unset
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
    let source = "before *strong\n日本語* and ``mono\r\ncode`` https://example.org[label\n続き]";
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
            style: crate::inline_model::InlineStyle::Strong,
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
    let source =
        ".Visible title\n[#item.role-a.role-b%collapsible,kind=\"demo\",positional]\nParagraph\n";
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
            AstBlock::Verbatim(block) if matches!(block.kind, VerbatimKind::Literal) => Some(block),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(literals.len(), 2);
    assert_eq!(literals[0].value, "<tag>\n*not strong*\n");
    assert_eq!(literals[1].value, "");
    assert!(literals.iter().all(|literal| literal.problems.is_empty()));
    assert_eq!(parsed.syntax.reconstruct(), source);
}

#[test]
fn literal_block_recovers_at_heading_when_unclosed() {
    let source = "....\ncontent\n== Next\nparagraph";
    let parsed = parse(source).expect("valid source");
    let AstBlock::Verbatim(literal) = &parsed.ast.blocks()[0] else {
        panic!("expected literal");
    };

    assert_eq!(literal.value, "content\n");
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
    assert_eq!(containers.len(), 4);
    assert!(matches!(
        containers[0].content,
        DelimitedContent::Verbatim(_)
    ));
    assert!(matches!(
        containers[1].content,
        DelimitedContent::Passthrough(_)
    ));
    assert!(matches!(containers[2].content, DelimitedContent::Table(_)));
    assert!(matches!(
        &containers[3].content,
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
        match (&parsed.ast.blocks()[0], expected) {
            (AstBlock::Verbatim(block), DelimitedBlockKind::Listing) => {
                assert!(matches!(block.kind, VerbatimKind::Listing));
            }
            (AstBlock::Verbatim(block), DelimitedBlockKind::Literal) => {
                assert!(matches!(block.kind, VerbatimKind::Literal));
            }
            (AstBlock::Delimited(block), expected) => {
                assert_eq!(block.kind, expected, "{delimiter}")
            }
            _ => panic!("{delimiter} must create the expected semantic block"),
        }
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
    let AstBlock::Verbatim(block) = &parsed.ast.blocks()[0] else {
        panic!("expected source block");
    };
    let VerbatimKind::Source(source_info) = &block.kind else {
        panic!("source kind");
    };

    assert_eq!(source_info.language.as_deref(), Some("rust"));
    let language_range = source_info.language_range.expect("language range");
    assert_eq!(
        &source[language_range.start().to_usize()..language_range.end().to_usize()],
        "rust"
    );
    assert_eq!(block.value, "fn main() {}\n");
    assert!(block.problems.is_empty());
    assert_eq!(parsed.syntax.reconstruct(), source);
}

#[test]
fn source_shorthand_and_document_default_normalize_to_verbatim_source() {
    let source = concat!(
        "= T\n",
        ":source-language: rust\n",
        "\n",
        "[,python]\n",
        "----\n",
        "print('ok')\n",
        "----\n",
        "\n",
        "----\n",
        "fn main() {}\n",
        "----\n",
        "\n",
        "[listing]\n",
        "----\n",
        "not source\n",
        "----\n",
    );
    let parsed = parse(source).expect("parse");

    let AstBlock::Verbatim(shorthand) = &parsed.ast.blocks()[1] else {
        panic!("shorthand source");
    };
    let VerbatimKind::Source(shorthand_info) = &shorthand.kind else {
        panic!("shorthand source kind");
    };
    assert_eq!(shorthand_info.language.as_deref(), Some("python"));
    assert_eq!(
        &source[shorthand_info
            .language_range
            .expect("range")
            .start()
            .to_usize()
            ..shorthand_info
                .language_range
                .expect("range")
                .end()
                .to_usize()],
        "python"
    );

    let AstBlock::Verbatim(defaulted) = &parsed.ast.blocks()[2] else {
        panic!("default source");
    };
    let VerbatimKind::Source(default_info) = &defaulted.kind else {
        panic!("default source kind");
    };
    assert_eq!(default_info.language.as_deref(), Some("rust"));
    assert_eq!(default_info.language_range, None);

    assert!(matches!(
        parsed.ast.blocks()[3],
        AstBlock::Verbatim(ref block) if matches!(block.kind, VerbatimKind::Listing)
    ));
}

#[test]
fn source_block_handles_missing_language_empty_and_unclosed() {
    let parsed = parse("[source]\n----\n== Next\n").expect("valid source");
    let AstBlock::Verbatim(block) = &parsed.ast.blocks()[0] else {
        panic!("expected source block");
    };
    let VerbatimKind::Source(source) = &block.kind else {
        panic!("source kind");
    };

    assert!(source.language.is_none());
    assert_eq!(block.value, "");
    assert!(block.problems.is_empty());
    assert!(
        parsed
            .syntax
            .issues()
            .iter()
            .any(|issue| { issue.class == crate::syntax::SyntaxIssueClass::MissingSourceLanguage })
    );
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
fn unsupported_source_options_are_not_misinterpreted() {
    let parsed = parse("[source,rust,unknown]\n----\ncode\n----\n").expect("lossless recovery");

    assert!(matches!(parsed.ast.blocks()[0], AstBlock::Unsupported(_)));
    assert!(matches!(
        parsed.ast.blocks()[1],
        AstBlock::Verbatim(ref block)
            if matches!(block.kind, VerbatimKind::Listing)
    ));
    assert!(parsed.syntax.issues().iter().any(|issue| {
        issue.class == crate::syntax::SyntaxIssueClass::InvalidAttribute
            && issue.message == "unsupported source block option"
    }));
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
        parsed
            .syntax
            .issues()
            .iter()
            .any(|issue| { issue.class == crate::syntax::SyntaxIssueClass::HeadingMarkerSpace })
    );
    assert!(
        parsed
            .syntax
            .issues()
            .iter()
            .any(|issue| { issue.class == crate::syntax::SyntaxIssueClass::InvalidHeadingLevel })
    );
    assert!(matches!(parsed.ast.blocks()[2], AstBlock::Paragraph(_)));
}

#[test]
fn paragraph_parser_matches_cst_and_ast_fixtures() {
    let source = include_str!("../../../../fixtures/paragraph/basic.adoc");
    let parsed = parse(source).expect("valid source");

    assert_eq!(
        parsed.syntax.snapshot(),
        include_str!("../../../../fixtures/paragraph/basic.syntax")
    );
    assert_eq!(
        parsed.ast.snapshot(),
        include_str!("../../../../fixtures/paragraph/basic.ast")
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
fn ordered_and_unordered_items_accept_multiline_principal_text() {
    for (source, kind, expected) in [
        (
            ". first\ncontinued first\n. second\ncontinued second\n",
            ListKind::Ordered,
            ["first\ncontinued first", "second\ncontinued second"],
        ),
        (
            ". first\n  continued first\n. second\n  continued second\n",
            ListKind::Ordered,
            ["first\n  continued first", "second\n  continued second"],
        ),
        (
            "* first\ncontinued first\n* second\ncontinued second\n",
            ListKind::Unordered,
            ["first\ncontinued first", "second\ncontinued second"],
        ),
        (
            "* first\n  continued first\n* second\n  continued second\n",
            ListKind::Unordered,
            ["first\n  continued first", "second\n  continued second"],
        ),
    ] {
        let parsed = parse(source).expect("multiline list");
        let [AstBlock::List(list)] = parsed.ast.blocks() else {
            panic!("one list");
        };
        assert_eq!(list.kind, kind);
        assert_eq!(list.items.len(), 2);
        assert_eq!(list.items[0].text, expected[0]);
        assert_eq!(list.items[1].text, expected[1]);
        assert!(
            list.items
                .iter()
                .all(|item| item.continuations.is_empty() && item.problems.is_empty())
        );
        assert_eq!(parsed.syntax.reconstruct(), source);
    }
}

#[test]
fn multiline_principal_text_stops_at_list_and_block_boundaries() {
    let source =
        ". parent\nparent body\n.. child\nchild body\n. sibling\nsibling body\n\noutside\n";
    let parsed = parse(source).expect("nested multiline list");
    let [AstBlock::List(list), AstBlock::Paragraph(outside)] = parsed.ast.blocks() else {
        panic!("list followed by an outside paragraph");
    };

    assert_eq!(list.items.len(), 2);
    assert_eq!(list.items[0].text, "parent\nparent body");
    assert_eq!(list.items[0].children.len(), 1);
    assert_eq!(list.items[0].children[0].items[0].text, "child\nchild body");
    assert_eq!(list.items[1].text, "sibling\nsibling body");
    assert_eq!(outside.value, "outside");
}

#[test]
fn multiline_principal_text_preserves_continuations_and_inline_ranges() {
    let source = "* principal\ncontinued xref:target[label]\n+\nattached paragraph\n* sibling\nsibling body\n";
    let parsed = parse(source).expect("multiline list with continuation");
    let [AstBlock::List(list)] = parsed.ast.blocks() else {
        panic!("one list");
    };
    let first = &list.items[0];

    assert_eq!(first.text, "principal\ncontinued xref:target[label]");
    assert_eq!(first.continuations.len(), 1);
    assert!(matches!(
        &first.continuations[0],
        AstBlock::Paragraph(paragraph) if paragraph.value == "attached paragraph"
    ));
    let reference = first
        .inlines
        .iter()
        .find_map(|inline| match inline {
            Inline::Reference(reference) => Some(reference),
            _ => None,
        })
        .expect("cross-reference on the continuation line");
    assert_eq!(
        &source[reference.target_range.start().to_usize()..reference.target_range.end().to_usize()],
        "target"
    );
    assert_eq!(list.items[1].text, "sibling\nsibling body");
}

#[test]
fn multiline_list_principal_text_resolves_an_explicit_hard_break() {
    let parsed = parse(". first +\ncontinued\n. second\n").expect("multiline list");
    let [AstBlock::List(list)] = parsed.ast.blocks() else {
        panic!("one list");
    };

    assert!(
        list.items[0]
            .inlines
            .iter()
            .any(|inline| matches!(inline, Inline::HardBreak { .. }))
    );
}

#[test]
fn ordered_list_presentation_is_resolved_during_lowering() {
    let parsed = parse("[start=3,%reversed,upperroman]\n. one\n. two\n").expect("parse");
    let AstBlock::List(list) = &parsed.ast.blocks()[0] else {
        panic!("ordered list");
    };

    assert_eq!(list.kind, ListKind::Ordered);
    assert_eq!(list.presentation.start, Some(3));
    assert!(list.presentation.reversed);
    assert_eq!(
        list.presentation.style,
        crate::block_model::OrderedListStyle::UpperRoman
    );
}

#[test]
fn explicit_ordered_numbers_set_start_and_preserve_the_marker_value() {
    let parsed = parse("4. four\n5. five\n").expect("parse");
    let AstBlock::List(list) = &parsed.ast.blocks()[0] else {
        panic!("ordered list");
    };

    assert_eq!(list.presentation.start, Some(4));
    assert_eq!(list.items[0].explicit_number, Some(4));
    assert_eq!(list.items[1].explicit_number, Some(5));
    assert!(list.presentation_problems.is_empty());
}

#[test]
fn invalid_explicit_ordered_numbers_remain_list_items() {
    let parsed = parse("4294967296. overflow\n0. zero\n").expect("parse");
    let AstBlock::List(list) = &parsed.ast.blocks()[0] else {
        panic!("ordered list");
    };

    assert_eq!(list.items.len(), 2);
    assert!(list.items.iter().all(|item| item.invalid_explicit_number));
    assert!(list.items.iter().all(|item| item.explicit_number.is_none()));
    assert_eq!(list.items[0].marker_range.end().to_u32(), 11);
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
        AstBlock::Verbatim(ref block) if matches!(block.kind, VerbatimKind::Literal)
    ));
    assert!(matches!(
        list.items[1].continuations[0],
        AstBlock::Verbatim(ref block) if matches!(block.kind, VerbatimKind::Source(_))
    ));
}

#[test]
fn list_continuation_uses_the_same_admonition_paragraph_model() {
    let source = "* item\n+\nWARNING:  text\n";
    let parsed = parse(source).expect("parse");
    let AstBlock::List(list) = &parsed.ast.blocks()[0] else {
        panic!("list");
    };
    let AstBlock::Paragraph(paragraph) = &list.items[0].continuations[0] else {
        panic!("continuation paragraph");
    };
    let admonition = paragraph.admonition.as_ref().expect("warning presentation");
    assert_eq!(admonition.kind, AdmonitionKind::Warning);
    assert_eq!(
        &source[admonition.label_range.start().to_usize()..admonition.label_range.end().to_usize()],
        "WARNING:"
    );
    assert_eq!(paragraph.inlines.len(), 1);
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
    assert!(description.items[0].problems.is_empty());

    let AstBlock::List(checklist) = &parsed.ast.blocks()[1] else {
        panic!("checklist");
    };
    assert_eq!(
        checklist.items[0].checklist,
        Some(ChecklistState::Unchecked)
    );
    assert_eq!(checklist.items[1].checklist, Some(ChecklistState::Checked));

    let AstBlock::Verbatim(source_block) = &parsed.ast.blocks()[2] else {
        panic!("source block");
    };
    let VerbatimKind::Source(_) = &source_block.kind else {
        panic!("source kind");
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
        parse(include_str!("../../../../fixtures/stem/substitutions.adoc")).expect("parse");
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
    assert_eq!(crate::document::document_symbols_ast(&parsed.ast).len(), 1);
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
fn empty_column_specs_preserve_the_explicit_semantic_table_shape() {
    let source = "[cols=\",,\",options=header]\n|===\n|ID |Check |Acceptance\n|REQ-001 |Automatic |Manual\n|===\n";
    let parsed = parse(source).expect("parse");
    let AstBlock::Delimited(block) = &parsed.ast.blocks()[0] else {
        panic!("table block")
    };
    let DelimitedContent::Table(table) = &block.content else {
        panic!("typed table")
    };
    assert_eq!(table.columns.len(), 3);
    assert!(table.columns.iter().enumerate().all(|(index, column)| {
        column.index == index as u32
            && column.width.is_none()
            && column.horizontal_alignment == crate::table::HorizontalAlignment::Left
            && column.vertical_alignment == crate::table::VerticalAlignment::Top
            && column.style == crate::table::TableCellStyle::Default
    }));
    assert_eq!(table.rows.len(), 2);
    assert_eq!(table.rows[0].section, crate::table::TableSection::Header);
    assert!(table.rows.iter().all(|row| row.cells.len() == 3));
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
            AstBlock::Delimited(crate::block_model::DelimitedBlock {
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
fn tables_infer_header_from_the_first_two_physical_lines() {
    let source = "\
|===
|Name |Value

|alpha |one
|===

[format=csv]
|===
name,value

alpha,one
|===

[format=dsv]
|===
name:value

alpha:one
|===

[format=tsv]
|===
name\tvalue

alpha\tone
|===
";
    let parsed = parse(source).expect("parse");
    let tables = parsed
        .ast
        .blocks()
        .iter()
        .filter_map(|block| match block {
            AstBlock::Delimited(crate::block_model::DelimitedBlock {
                content: DelimitedContent::Table(table),
                ..
            }) => Some(table),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(tables.len(), 4);
    for table in tables {
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[0].section, crate::table::TableSection::Header);
        assert_eq!(table.rows[1].section, crate::table::TableSection::Body);
    }
}

#[test]
fn explicit_table_header_options_override_automatic_inference() {
    let source = "\
[%noheader]
|===
|Name |Value

|alpha |one
|===

[%header,cols=2]
|===
|Name
|Value
|alpha
|one
|===

[cols=2]
|===
|Name
|Value

|alpha
|one
|===

[%header%noheader]
|===
|Name |Value

|alpha |one
|===
";
    let parsed = parse(source).expect("parse");
    let tables = parsed
        .ast
        .blocks()
        .iter()
        .filter_map(|block| match block {
            AstBlock::Delimited(crate::block_model::DelimitedBlock {
                content: DelimitedContent::Table(table),
                ..
            }) => Some(table),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(tables.len(), 4);
    assert_eq!(tables[0].rows[0].section, crate::table::TableSection::Body);
    assert_eq!(
        tables[1].rows[0].section,
        crate::table::TableSection::Header
    );
    assert_eq!(tables[2].rows[0].section, crate::table::TableSection::Body);
    assert_eq!(
        tables[3].rows[0].section,
        crate::table::TableSection::Header
    );
}

#[test]
fn table_header_inference_ignores_comments_and_later_wider_rows() {
    let source = "\
|===
   // before the header
|H1 |H2
/// between the header and separator

|a |b |c
|===

[format=csv]
|===
	// before the header
h1,h2
/// between the header and separator

a,b
|===
";
    let parsed = parse(source).expect("parse");
    let tables = parsed
        .ast
        .blocks()
        .iter()
        .filter_map(|block| match block {
            AstBlock::Delimited(crate::block_model::DelimitedBlock {
                content: DelimitedContent::Table(table),
                ..
            }) => Some(table),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(tables.len(), 2);
    assert_eq!(tables[0].columns.len(), 2);
    assert_eq!(
        tables[0].rows[0].section,
        crate::table::TableSection::Header
    );
    assert_eq!(tables[0].rows[0].cells[0].raw, "H1");
    assert_eq!(tables[0].rows[0].cells[1].raw, "H2");
    assert_eq!(
        tables[1].rows[0].section,
        crate::table::TableSection::Header
    );
    assert_eq!(tables[1].rows[0].cells[0].raw, "h1");
    assert_eq!(parsed.syntax.reconstruct(), source);
}

#[test]
fn separated_table_blank_records_do_not_shift_later_rows() {
    let source = "\
[format=csv]
|===
name,value
alpha,one

beta,two
|===
";
    let parsed = parse(source).expect("parse");
    let table = parsed
        .ast
        .blocks()
        .iter()
        .find_map(|block| match block {
            AstBlock::Delimited(crate::block_model::DelimitedBlock {
                content: DelimitedContent::Table(table),
                ..
            }) => Some(table),
            _ => None,
        })
        .expect("table");
    assert_eq!(table.rows.len(), 3);
    assert_eq!(table.rows[2].cells[0].raw, "beta");
    assert_eq!(table.rows[2].cells[1].raw, "two");
}

#[test]
fn noheader_keeps_standard_first_record_column_inference() {
    let source = "\
[%noheader,format=csv]
|===
h1,h2

a,b,c
|===
";
    let parsed = parse(source).expect("parse");
    let table = parsed
        .ast
        .blocks()
        .iter()
        .find_map(|block| match block {
            AstBlock::Delimited(crate::block_model::DelimitedBlock {
                content: DelimitedContent::Table(table),
                ..
            }) => Some(table),
            _ => None,
        })
        .expect("table");
    assert_eq!(table.columns.len(), 2);
    assert_eq!(table.rows[0].section, crate::table::TableSection::Body);
    assert_eq!(table.rows[0].cells[0].raw, "h1");
    assert_eq!(table.rows[0].cells[1].raw, "h2");
}

#[test]
fn header_comment_handling_uses_the_effective_psv_column_style() {
    let source = "\
[cols=\"a,d\"]
|===
|H1 |H2
// table comment

|a |b
|===

[cols=\"d,a\"]
|===
|H1 |H2
// AsciiDoc cell content

|a |b
|===
";
    let parsed = parse(source).expect("parse");
    let tables = parsed
        .ast
        .blocks()
        .iter()
        .filter_map(|block| match block {
            AstBlock::Delimited(crate::block_model::DelimitedBlock {
                content: DelimitedContent::Table(table),
                ..
            }) => Some(table),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(tables.len(), 2);
    assert_eq!(
        tables[0].rows[0].section,
        crate::table::TableSection::Header
    );
    assert_eq!(tables[0].rows[0].cells[1].raw, "H2");
    assert_eq!(tables[1].rows[0].section, crate::table::TableSection::Body);
    assert_eq!(
        tables[1].rows[0].cells[1].raw,
        "H2\n// AsciiDoc cell content"
    );
}

#[test]
fn asciidoc_table_cells_are_parsed_as_nested_blocks() {
    crate::source::SourceDocument::reset_construction_count();
    let source = "[cols=a]\n|===\n|A paragraph.\n\n* one\n* two\n|===\n";
    let parsed = parse(source).expect("parse");
    assert_eq!(
        crate::source::SourceDocument::construction_count(),
        1,
        "source-backed PSV cells must not rebuild the line index"
    );
    assert_eq!(crate::source::SourceDocument::indexed_view_count(), 1);
    let AstBlock::Delimited(crate::block_model::DelimitedBlock {
        content: DelimitedContent::Table(table),
        ..
    }) = &parsed.ast.blocks()[0]
    else {
        panic!("table")
    };
    let crate::table::TableCellContent::AsciiDoc(blocks) = &table.rows[0].cells[0].content else {
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
    let AstBlock::Delimited(crate::block_model::DelimitedBlock {
        content: DelimitedContent::Table(table),
        ..
    }) = &parsed.ast.blocks()[0]
    else {
        panic!("table")
    };
    let crate::table::TableCellContent::AsciiDoc(blocks) = &table.rows[0].cells[0].content else {
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
        AstBlock::Verbatim(VerbatimBlock {
            kind: VerbatimKind::Source(SourceInfo { language, .. }),
            metadata,
            ..
        })
            if language.as_deref() == Some("rust")
                && metadata.title.as_ref().map(|title| title.value.as_str())
                    == Some("Cell source")
    ));
    assert!(matches!(blocks[5], AstBlock::Math(_)));
    let AstBlock::Delimited(crate::block_model::DelimitedBlock {
        content: DelimitedContent::Table(nested),
        ..
    }) = &blocks[6]
    else {
        panic!("nested table")
    };
    assert_eq!(nested.separator, '!');
    assert_eq!(nested.rows[0].cells[0].raw, "nested");
    assert!(parsed.ast.anchors().iter().any(|anchor| {
        anchor.id == "cell-target" && anchor.valid && anchor.target_range == Some(blocks[0].range())
    }));
    assert!(
        crate::document::reference_targets_ast(&parsed.ast)
            .iter()
            .any(|target| {
                target.id == "cell-target" && target.target_range == blocks[0].range()
            })
    );
    assert!(
        crate::html::render_with_inputs_ast(
            &parsed.ast,
            &crate::html::RenderPolicy::default(),
            &crate::render::RenderInputs::default(),
        )
        .html
        .contains("<h1 id=\"cell-target\">Cell heading</h1>")
    );
    assert_eq!(parsed.syntax.reconstruct(), source);
}

#[test]
fn asciidoc_cell_syntax_problems_join_the_root_diagnostic_stream() {
    let parsed = parse("[cols=a]\n|===\n|[source]\n----\ncode\n----\n|===\n").expect("parse");
    assert!(
        parsed
            .syntax
            .issues()
            .iter()
            .any(|issue| { issue.class == crate::syntax::SyntaxIssueClass::MissingSourceLanguage })
    );
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
        let AstBlock::Delimited(crate::block_model::DelimitedBlock {
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
                        AstBlock::Verbatim(VerbatimBlock {
                            kind: VerbatimKind::Literal,
                            ..
                        })
                    )
                    | (
                        Expected::Source,
                        AstBlock::Verbatim(VerbatimBlock {
                            kind: VerbatimKind::Source(_),
                            ..
                        })
                    )
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
