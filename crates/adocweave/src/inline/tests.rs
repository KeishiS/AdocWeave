use super::{
    AnalysisLimits, DelimiterIndex, FormulaToken, Inline, InlineCandidate, InlineCandidateIndex,
    InlineLiteralKind, InlineParseConfig, InlineProblem, InlineProblemKind, InlineRecognition,
    InlineStyle, InlineToken, LinkToken, MacroForm, MacroToken, MarkerForm, MarkerToken,
    ReferenceDestination, ReferenceToken, StandardMacroKind, inline_at, is_plain_inline_text,
    next_candidate, parse, parse_text, recognize_macro, recognize_marker,
};
use crate::source::{TextRange, TextSize};

fn range(start: usize, end: usize) -> TextRange {
    TextRange::new(
        TextSize::new(start).expect("small offset"),
        TextSize::new(end).expect("small offset"),
    )
    .expect("ordered range")
}

#[test]
fn plain_inline_query_uses_the_document_recognizer_and_a_tight_budget() {
    for value in ["日本語です。", "C++ APIです。", "A * Bです。"] {
        assert!(
            is_plain_inline_text(value),
            "plain text was rejected: {value}"
        );
    }
    for value in [
        "",
        "https://example.com/path",
        "user@example.com",
        "{name}",
        "**強調**です。",
        "`code`",
        "pass:[raw]",
    ] {
        assert!(
            !is_plain_inline_text(value),
            "inline syntax was accepted: {value}"
        );
    }
    let boundary = "a".repeat(AnalysisLimits::default().max_line_bytes as usize);
    assert!(is_plain_inline_text(&boundary));
    assert!(!is_plain_inline_text(&format!("{boundary}a")));
}

#[test]
fn inline_text_preserves_source_range_and_unicode() {
    let inlines = parse_text("日本語 😀", range(4, 18), InlineParseConfig::default());
    let Inline::Text(text) = &inlines[0] else {
        panic!("expected text");
    };
    assert_eq!(text.value, "日本語 😀");
    assert_eq!(text.range, range(4, 18));
    assert_eq!(inline_at(&inlines, 6), Some(&inlines[0]));
    assert_eq!(inline_at(&inlines, 18), None);
}

#[test]
fn inline_text_handles_empty_input() {
    assert!(parse_text("", range(0, 0), InlineParseConfig::default()).is_empty());
}

#[test]
fn recognizer_orders_macros_and_markers_by_source_position() {
    assert_eq!(
        next_candidate("*strong* https://example.org", 0),
        Some(InlineCandidate::Marker {
            open: 0,
            marker: '*',
            form: MarkerForm::Constrained,
            close: Some(7),
        })
    );
    assert_eq!(
        next_candidate("https://example.org *strong*", 0),
        Some(InlineCandidate::Macro { open: 0 })
    );
    assert_eq!(
        next_candidate("日本語 xref:other.adoc[]", "日本語 ".len()),
        Some(InlineCandidate::Macro {
            open: "日本語 ".len()
        })
    );
}

#[test]
fn candidate_index_has_fixed_linear_inspection_and_storage_budgets() {
    fn assert_bounded(source: &str) {
        let index = InlineCandidateIndex::new(source);
        assert!(index.inspected_positions() <= source.len().saturating_mul(12));
        assert!(
            index.storage_bytes() <= source.len().saturating_add(1).saturating_mul(128),
            "candidate index storage must remain linear"
        );
    }

    assert_eq!(InlineCandidateIndex::new("abc").inspected_positions(), 29);

    let source = "日本語 *open xref:broken[ https://example.org[label] _tail";
    let index = InlineCandidateIndex::new(source);

    assert!(index.inspected_positions() > source.len());
    assert_bounded(source);

    for repetitions in 1..128 {
        let hostile = "xref:".repeat(repetitions) + "target[open";
        assert_bounded(&hostile);
        let output = parse(
            &hostile,
            range(0, hostile.len()),
            InlineParseConfig::default(),
        );
        assert!(output.problems.len() <= 1);
    }
    for repetitions in 1..128 {
        let hostile = "\"`x ".repeat(repetitions);
        assert_bounded(&hostile);
    }
    let seed = include_str!("../../../../fixtures/lint/macro-boundary-adversarial.adoc");
    let hostile = seed.repeat(256);
    assert_bounded(&hostile);
}

#[test]
fn candidate_index_is_immutable_and_each_cursor_advances_independently() {
    let index = InlineCandidateIndex::new("*first* xref:target[]");
    let mut first = index.cursor();
    let mut second = index.cursor();

    assert_eq!(first.next(0), second.next(0));
    assert_eq!(first.next("*first* xref:target[]".len()), None);
    assert_eq!(
        second.next("*first* ".len()),
        Some(InlineCandidate::Macro {
            open: "*first* ".len()
        })
    );
}

#[test]
fn scanner_delimiter_indexes_use_compact_offsets() {
    assert_eq!(std::mem::size_of::<u32>(), 4);
    let source = "[x] <<target>> ".repeat(1_024);
    let index = DelimiterIndex::new(&source);
    assert_eq!(index.storage_bytes(), (source.len() + 1) * 3 * 4);
    assert!(index.storage_bytes() <= source.len() * 13);
}

#[test]
fn macro_recognizer_returns_ranges_without_building_nodes() {
    assert!(matches!(
        recognize_macro("stem:[x]", 0),
        InlineRecognition::Matched(InlineToken::Macro(MacroToken::Formula(FormulaToken {
            content_start: 6,
            content_end: 7,
            end: 8,
            closed: true,
            ..
        })))
    ));
    assert!(matches!(
        recognize_macro("<<id,label>>", 0),
        InlineRecognition::Matched(InlineToken::Macro(MacroToken::Reference(
            ReferenceToken::Short {
                target_start: 2,
                close: 10,
                end: 12,
                ..
            }
        )))
    ));
    assert!(matches!(
        recognize_macro("xref:other.adoc[Other]", 0),
        InlineRecognition::Matched(InlineToken::Macro(MacroToken::Reference(
            ReferenceToken::Xref {
                target_start: 5,
                bracket: 15,
                close: 21,
                end: 22,
                ..
            }
        )))
    ));
    assert!(matches!(
        recognize_macro("https://example.org[label]", 0),
        InlineRecognition::Matched(InlineToken::Macro(MacroToken::Link(LinkToken::Url {
            target_end: 19,
            label: Some((20, 25)),
            end: 26,
            ..
        })))
    ));
    assert!(matches!(
        recognize_macro("image:asset.png[Alt]", 0),
        InlineRecognition::Matched(InlineToken::Macro(MacroToken::Standard(
            super::StandardMacroToken {
                kind: StandardMacroKind::Image,
                form: MacroForm::Inline,
                target_start: 6,
                bracket: 15,
                close: 19,
                end: 20,
                ..
            }
        )))
    ));
    assert_eq!(
        recognize_macro("xref:other.adoc[open", 0),
        InlineRecognition::Recovered {
            open: 0,
            kind: InlineProblemKind::IncompleteCrossReference,
            next: 1,
        }
    );
    assert_eq!(
        recognize_macro("https://example.org[open", 0),
        InlineRecognition::Recovered {
            open: 0,
            kind: InlineProblemKind::IncompleteLink,
            next: 1,
        }
    );
}

#[test]
fn marker_recognizer_distinguishes_complete_invalid_and_unclosed_input() {
    assert_eq!(
        recognize_marker("*strong*", 0, '*', MarkerForm::Constrained, Some(7),),
        InlineRecognition::Matched(InlineToken::Marker(MarkerToken {
            open: 0,
            close: 7,
            end: 8,
            marker: '*',
            form: MarkerForm::Constrained,
        }))
    );
    assert_eq!(
        recognize_marker("{bad name}", 0, '{', MarkerForm::Constrained, Some(9),),
        InlineRecognition::Rejected { open: 0, next: 1 }
    );
    assert_eq!(
        recognize_marker("_open", 0, '_', MarkerForm::Constrained, None),
        InlineRecognition::Recovered {
            open: 0,
            next: 1,
            kind: InlineProblemKind::UnclosedEmphasis,
        }
    );
}

#[test]
fn selected_semantic_lowering_is_isolated_from_recognition() {
    const RECOGNITION: &str = include_str!("../inline.rs");
    const LOWERING: &str = include_str!("lowering.rs");

    for function in ["lower_marker", "lower_reference", "lower_standard_macro"] {
        assert!(LOWERING.contains(&format!("fn {function}(")));
        assert!(RECOGNITION.contains(&format!("lowering::{function}(")));
    }
    for constructor in ["Inline::Styled", "Inline::Reference", "Inline::Macro"] {
        assert!(LOWERING.contains(constructor));
    }
    for recognition_detail in [
        "fn recognize_",
        "InlineRecognition",
        "InlineCandidateIndex",
        "DelimiterIndex",
    ] {
        assert!(!LOWERING.contains(recognition_detail));
    }
    for old_builder in ["marker", "reference_macro", "standard_macro"] {
        assert!(!RECOGNITION.contains(&format!("fn build_{old_builder}(")));
    }
}

#[test]
fn marker_reference_and_macro_lowering_preserve_utf8_ranges_deterministically() {
    fn source_slice<'a>(source: &'a str, base: usize, inline: &Inline) -> &'a str {
        let range = inline.range();
        let start = range.start().to_usize() - base;
        let end = range.end().to_usize() - base;
        assert!(start < end && end <= source.len());
        assert!(source.is_char_boundary(start));
        assert!(source.is_char_boundary(end));
        &source[start..end]
    }

    for fragment in ["a", "日本", "😀", "a-b_1", "é"] {
        let marker = format!("*{fragment}*");
        let reference = format!("xref:doc#{fragment}[_{fragment}_]");
        let macro_source = format!("image:{fragment}.png[Alt,{fragment}]");
        let source = format!("{marker} {reference} {macro_source}");
        let base = 7;
        let source_range = range(base, base + source.len());
        let first = parse(&source, source_range, InlineParseConfig::default());
        let second = parse(&source, source_range, InlineParseConfig::default());

        assert_eq!(first, second);
        assert!(first.problems.is_empty(), "{source:?}");
        assert_eq!(first.inlines.len(), 5);
        assert!(matches!(first.inlines[0], Inline::Styled { .. }));
        assert!(matches!(first.inlines[2], Inline::Reference(_)));
        assert!(matches!(first.inlines[4], Inline::Macro(_)));
        assert_eq!(source_slice(&source, base, &first.inlines[0]), marker);
        assert_eq!(source_slice(&source, base, &first.inlines[2]), reference);
        assert_eq!(source_slice(&source, base, &first.inlines[4]), macro_source);
        for inline in &first.inlines {
            let _ = source_slice(&source, base, inline);
        }
    }
}

#[test]
fn candidate_recovery_always_advances_on_utf8_boundaries() {
    for source in [
        "日本語 xref:broken[ *open _also",
        "link:https://example.org[Label] image:asset.png[Alt]",
        "<<target,label>> https://example.org[label] user@example.org",
        "{bad name} **strong** stem:[x]",
    ] {
        let index = InlineCandidateIndex::new(source);
        let mut candidates = index.cursor();
        let mut cursor = 0;
        let mut steps = 0;
        while let Some(candidate) = candidates.next(cursor) {
            let recognition = index.recognize(source, candidate);
            let next = recognition.map_or_else(
                || super::next_char_boundary(source, candidate.open()),
                |recognition| {
                    assert!(recognition.is_well_formed(source));
                    assert_eq!(
                        Some(recognition),
                        index.recognize(source, candidate),
                        "recognition must be deterministic"
                    );
                    recognition.next()
                },
            );
            assert!(next > cursor, "{source:?} at {cursor}");
            assert!(source.is_char_boundary(next));
            cursor = next;
            steps += 1;
        }
        assert!(steps <= source.chars().count());
    }
}

#[test]
fn links_keep_target_label_and_source_ranges_separate() {
    let source = "see https://example.com[*site*].";
    let output = parse(source, range(10, 42), InlineParseConfig::default());
    let Inline::Link(link) = &output.inlines[1] else {
        panic!("expected link");
    };
    assert_eq!(link.target_source, "https://example.com");
    assert_eq!(link.target, "https://example.com");
    assert_eq!(
        &source[link.target_range.start().to_usize() - 10..link.target_range.end().to_usize() - 10],
        "https://example.com"
    );
    assert!(matches!(
        link.label[0],
        Inline::Styled {
            style: InlineStyle::Strong,
            ..
        }
    ));
    assert!(output.problems.is_empty());
}

#[test]
fn macro_labels_propagate_nested_inline_problems() {
    for (source, expected) in [
        (
            "https://example.com[*open]",
            InlineProblemKind::UnclosedStrong,
        ),
        (
            "xref:other.adoc[_open]",
            InlineProblemKind::UnclosedEmphasis,
        ),
        ("<<target,`open>>", InlineProblemKind::UnclosedMonospace),
    ] {
        let output = parse(source, range(0, source.len()), InlineParseConfig::default());
        assert!(
            output
                .problems
                .iter()
                .any(|problem| problem.kind == expected),
            "missing {expected:?} for {source:?}"
        );
    }
}

#[test]
fn escaped_macros_do_not_report_literal_contents_as_syntax() {
    for (source, expected) in [("\\stem:[", "stem:["), ("\\xref:broken[", "xref:broken[")] {
        let output = parse(source, range(0, source.len()), InlineParseConfig::default());
        assert!(output.problems.is_empty());
        assert!(matches!(
            output.inlines.as_slice(),
            [Inline::Text(text)] if text.value == expected
        ));
    }
}

#[test]
fn escaped_markers_are_literal_without_the_escape_character() {
    for (source, expected) in [
        ("\\*strong*", "*strong*"),
        ("\\_emphasis_", "_emphasis_"),
        ("\\`mono`", "`mono`"),
        ("\\{name}", "{name}"),
        ("before \\*open", "before *open"),
    ] {
        let output = parse(source, range(0, source.len()), InlineParseConfig::default());
        let visible = output
            .inlines
            .iter()
            .map(|inline| match inline {
                Inline::Text(text) => text.value.as_str(),
                _ => panic!("escaped syntax must remain text: {source}"),
            })
            .collect::<String>();
        assert_eq!(visible, expected);
        assert!(output.problems.is_empty());
    }
}

#[test]
fn escaped_anchor_openers_are_literal_text() {
    for (source, expected) in [("\\[[id]]", "[[id]]"), ("\\[#id]", "[#id]")] {
        let output = parse(source, range(0, source.len()), InlineParseConfig::default());
        let visible = output
            .inlines
            .iter()
            .map(|inline| match inline {
                Inline::Text(text) => text.value.as_str(),
                _ => panic!("escaped anchor must remain text"),
            })
            .collect::<String>();
        assert_eq!(visible, expected);
        assert!(output.problems.is_empty());
    }
}

#[test]
fn backslash_runs_and_trailing_backslashes_recover_deterministically() {
    let trailing = parse("text\\", range(0, 5), InlineParseConfig::default());
    assert!(matches!(
        trailing.inlines.as_slice(),
        [Inline::Text(text)] if text.value == "text\\"
    ));

    let even = parse("\\\\*strong*", range(0, 10), InlineParseConfig::default());
    assert!(matches!(even.inlines[1], Inline::Styled { .. }));
    assert!(matches!(&even.inlines[0], Inline::Text(text) if text.value == "\\\\"));

    let odd = parse("\\\\\\*strong*", range(0, 11), InlineParseConfig::default());
    assert!(
        odd.inlines
            .iter()
            .all(|inline| matches!(inline, Inline::Text(_)))
    );
    let visible = odd
        .inlines
        .iter()
        .filter_map(|inline| match inline {
            Inline::Text(text) => Some(text.value.as_str()),
            _ => None,
        })
        .collect::<String>();
    assert_eq!(visible, "\\\\*strong*");
}

#[test]
fn escapes_are_not_interpreted_inside_opaque_inline_contexts() {
    let source = "`\\*literal*` stem:[\\{x}]";
    let output = parse(source, range(0, source.len()), InlineParseConfig::default());

    assert!(matches!(
        &output.inlines[0],
        Inline::Literal { value, .. } if value == "\\*literal*"
    ));
    assert!(matches!(
        &output.inlines[2],
        Inline::Formula(formula) if formula.value == "\\{x}"
    ));
    assert!(output.problems.is_empty());
}

#[test]
fn cross_references_share_one_typed_model() {
    let source = concat!(
        "<<local,Local>> ",
        "xref:#local[] ",
        "xref:other.adoc#part[Other] ",
        "xref:note:123#part[Note]"
    );
    let output = parse(source, range(0, source.len()), InlineParseConfig::default());
    let references = output
        .inlines
        .iter()
        .filter_map(|inline| match inline {
            Inline::Reference(reference) => Some(reference),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(references.len(), 4);
    assert!(matches!(
        references[0].authored_destination,
        ReferenceDestination::Local { ref anchor, .. } if anchor == "local"
    ));
    assert!(matches!(
        references[2].authored_destination,
        ReferenceDestination::Document { ref document, ref anchor, .. }
            if document == "other.adoc" && anchor.as_deref() == Some("part")
    ));
    assert!(matches!(
        references[3].authored_destination,
        ReferenceDestination::Scheme { ref scheme, ref locator, .. }
            if scheme == "note" && locator == "123"
    ));
}

#[test]
fn standard_macros_share_target_attribute_and_range_model() {
    let source =
        "image::https://example.org/a.png[Alt,320,height=200] footnote:[note] user@example.org";
    let parsed = parse(source, range(0, source.len()), InlineParseConfig::default());
    let macros = parsed
        .inlines
        .iter()
        .filter_map(|inline| match inline {
            Inline::Macro(node) => Some(node),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(macros.len(), 3);
    assert_eq!(macros[0].kind, StandardMacroKind::Image);
    assert_eq!(macros[0].form, MacroForm::Block);
    assert_eq!(macros[0].attributes[0].value, "Alt");
    assert_eq!(macros[0].attributes[2].name.as_deref(), Some("height"));
    assert_eq!(macros[1].kind, StandardMacroKind::Footnote);
    assert_eq!(macros[2].kind, StandardMacroKind::Email);
}

#[test]
fn links_and_cross_references_support_backslash_escape_and_recovery() {
    let source = "\\https://example.com[x] xref:broken[ then `code`";
    let output = parse(source, range(0, source.len()), InlineParseConfig::default());
    let visible_text = output
        .inlines
        .iter()
        .filter_map(|inline| match inline {
            Inline::Text(text) => Some(text.value.as_str()),
            _ => None,
        })
        .collect::<String>();
    assert_eq!(visible_text, "https://example.com[x] xref:broken[ then ");
    assert!(output.inlines.iter().any(|inline| matches!(
        inline,
        Inline::Literal { value, .. } if value == "code"
    )));
    assert!(
        output
            .problems
            .iter()
            .any(|problem| problem.kind == InlineProblemKind::IncompleteCrossReference)
    );
}

#[test]
fn incomplete_macro_detection_ignores_brackets_before_the_macro() {
    let source = "] https://example.com[open";
    let output = parse(source, range(0, source.len()), InlineParseConfig::default());

    assert!(
        output
            .problems
            .iter()
            .any(|problem| problem.kind == InlineProblemKind::IncompleteLink)
    );
}

#[test]
fn monospace_parses_multiple_spans_and_ranges() {
    let output = parse(
        "a `one` and `二`",
        range(10, 27),
        InlineParseConfig::default(),
    );
    assert_eq!(output.inlines.len(), 4);
    assert!(matches!(
        &output.inlines[1],
        Inline::Literal {
            kind: InlineLiteralKind::Monospace,
            value,
            ..
        } if value == "one"
    ));
    assert!(matches!(
        &output.inlines[3],
        Inline::Literal {
            kind: InlineLiteralKind::Monospace,
            value,
            ..
        } if value == "二"
    ));
    assert!(output.problems.is_empty());
}

#[test]
fn monospace_unclosed_input_recovers_as_text() {
    let output = parse("before `open", range(0, 12), InlineParseConfig::default());
    assert_eq!(output.inlines.len(), 1);
    assert!(matches!(&output.inlines[0], Inline::Text(text) if text.value == "before `open"));
    assert_eq!(
        output.problems[0].kind,
        InlineProblemKind::UnclosedMonospace
    );
    assert_eq!(output.problems[0].range, range(7, 8));
}

#[test]
fn monospace_reports_invalid_constrained_boundaries() {
    let output = parse(
        "word`code`word and ``",
        range(0, 20),
        InlineParseConfig::default(),
    );
    assert!(
        output
            .inlines
            .iter()
            .all(|inline| matches!(inline, Inline::Text(_)))
    );
    assert_eq!(
        output.problems,
        [InlineProblem {
            kind: InlineProblemKind::MonospaceBoundary,
            range: range(4, 10),
        }]
    );
}

#[test]
fn constrained_monospace_rejects_standard_word_and_opening_boundaries() {
    for source in [
        "snake_`code`",
        "key:`code`",
        "key;`code`",
        "x}`code`",
        "日本`code`",
        "１`code`",
    ] {
        let output = parse(source, range(0, source.len()), InlineParseConfig::default());
        assert!(
            output
                .inlines
                .iter()
                .all(|inline| matches!(inline, Inline::Text(_))),
            "{source:?} unexpectedly contained formatted inline content"
        );
        assert_eq!(
            output.problems.len(),
            1,
            "{source:?} must report one boundary problem"
        );
        assert_eq!(
            output.problems[0].kind,
            InlineProblemKind::MonospaceBoundary,
            "{source:?}"
        );
    }

    let output = parse(
        "`code`_tail",
        range(0, "`code`_tail".len()),
        InlineParseConfig::default(),
    );
    assert!(
        output
            .inlines
            .iter()
            .all(|inline| matches!(inline, Inline::Text(_)))
    );
    assert_eq!(output.problems.len(), 2);
    assert_eq!(
        output
            .problems
            .iter()
            .map(|problem| (problem.kind, problem.range))
            .collect::<Vec<_>>(),
        [
            (InlineProblemKind::MonospaceBoundary, range(0, 6)),
            (InlineProblemKind::UnclosedEmphasis, range(6, 7)),
        ]
    );

    let output = parse(
        "`code`日本",
        range(0, "`code`日本".len()),
        InlineParseConfig::default(),
    );
    assert!(
        output
            .inlines
            .iter()
            .all(|inline| matches!(inline, Inline::Text(_)))
    );
    assert_eq!(output.problems.len(), 1);
    assert_eq!(
        (output.problems[0].kind, output.problems[0].range),
        (InlineProblemKind::MonospaceBoundary, range(0, 6))
    );
}

#[test]
fn monospace_boundary_diagnostic_preserves_valid_and_protected_forms() {
    for source in [
        "(`code`)",
        "before `code` after",
        "日本語``code``日本語",
        r"日本語\`code\`日本語",
        r#""`quoted`""#,
        "+日本語`code`日本語+",
    ] {
        let output = parse(source, range(0, source.len()), InlineParseConfig::default());
        assert!(output.problems.is_empty(), "{source:?}: {output:#?}");
    }
}

#[test]
fn monospace_boundary_diagnostic_replaces_derived_unclosed_problem() {
    for source in [
        "ファイル`pbmc_processed.h5ad`を",
        "AnnDataの`obs[\"predicted.celltype.l1\"]`を",
        "key:`code`",
        "before ` code ` after",
    ] {
        let output = parse(source, range(0, source.len()), InlineParseConfig::default());
        assert_eq!(output.problems.len(), 1, "{source:?}: {output:#?}");
        assert_eq!(
            output.problems[0].kind,
            InlineProblemKind::MonospaceBoundary,
            "{source:?}"
        );
    }
}

#[test]
fn constrained_monospace_accepts_punctuation_and_unconstrained_ignores_boundaries() {
    let source =
        "key-`code` snake_``under`` key:``colon`` x}``brace`` 日本``和文``日本 😀``emoji``😀";
    let output = parse(source, range(0, source.len()), InlineParseConfig::default());
    let values = output
        .inlines
        .iter()
        .filter_map(|inline| match inline {
            Inline::Literal {
                kind: InlineLiteralKind::Monospace,
                value,
                ..
            } => Some(value.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(values, ["code", "under", "colon", "brace", "和文", "emoji"]);
    assert!(output.problems.is_empty());
}

#[test]
fn unconstrained_markers_work_inside_words_and_across_unicode_boundaries() {
    let source = "word**strong**word 日本語__強調__日本語 😀``code``😀";
    let output = parse(source, range(0, source.len()), InlineParseConfig::default());

    assert!(output.problems.is_empty());
    assert!(output.inlines.iter().any(|inline| {
        matches!(inline, Inline::Styled { style: InlineStyle::Strong, children, .. }
            if matches!(&children[..], [Inline::Text(text)] if text.value == "strong"))
    }));
    assert!(output.inlines.iter().any(|inline| {
        matches!(inline, Inline::Styled { style: InlineStyle::Emphasis, children, .. }
            if matches!(&children[..], [Inline::Text(text)] if text.value == "強調"))
    }));
    assert!(output.inlines.iter().any(|inline| {
        matches!(inline, Inline::Literal { kind: InlineLiteralKind::Monospace, value, .. }
            if value == "code")
    }));
}

#[test]
fn unconstrained_styles_nest_and_adjacent_pairs_remain_deterministic() {
    let source = "**outer __inner__** **one****two**";
    let output = parse(source, range(0, source.len()), InlineParseConfig::default());

    assert!(output.problems.is_empty());
    let styled: Vec<_> = output
        .inlines
        .iter()
        .filter_map(|inline| match inline {
            Inline::Styled { children, .. } => Some(children),
            _ => None,
        })
        .collect();
    assert_eq!(styled.len(), 3);
    assert!(styled[0].iter().any(|inline| matches!(
        inline,
        Inline::Styled {
            style: InlineStyle::Emphasis,
            ..
        }
    )));
    assert!(matches!(&styled[1][..], [Inline::Text(text)] if text.value == "one"));
    assert!(matches!(&styled[2][..], [Inline::Text(text)] if text.value == "two"));
}

#[test]
fn unconstrained_empty_and_escaped_pairs_stay_literal() {
    let source = "**** ____ `` \\**literal**";
    let output = parse(source, range(0, source.len()), InlineParseConfig::default());
    let visible = output
        .inlines
        .iter()
        .map(|inline| match inline {
            Inline::Text(text) => text.value.as_str(),
            _ => panic!("expected only literal text"),
        })
        .collect::<String>();

    assert_eq!(visible, "**** ____ `` **literal**");
    assert!(output.problems.is_empty());
}

#[test]
fn strong_parses_content_and_nested_monospace() {
    let output = parse(
        "a *strong `code` text* end",
        range(0, 26),
        InlineParseConfig::default(),
    );
    let Inline::Styled {
        style: InlineStyle::Strong,
        children,
        ..
    } = &output.inlines[1]
    else {
        panic!("expected strong");
    };
    assert!(children.iter().any(|inline| matches!(
        inline,
        Inline::Literal {
            kind: InlineLiteralKind::Monospace,
            value,
            ..
        } if value == "code"
    )));
    assert!(output.problems.is_empty());
}

#[test]
fn strong_unclosed_marker_does_not_hide_later_monospace() {
    let output = parse(
        "*open then `code`",
        range(0, 17),
        InlineParseConfig::default(),
    );
    assert!(output.inlines.iter().any(|inline| matches!(
        inline,
        Inline::Literal {
            kind: InlineLiteralKind::Monospace,
            ..
        }
    )));
    assert!(
        output
            .problems
            .iter()
            .any(|problem| problem.kind == InlineProblemKind::UnclosedStrong)
    );
}

#[test]
fn strong_handles_multiple_spans_and_leaves_empty_markers_as_text() {
    let output = parse(
        "*one* and *two* plus **",
        range(0, 23),
        InlineParseConfig::default(),
    );

    assert_eq!(
        output
            .inlines
            .iter()
            .filter(|inline| matches!(
                inline,
                Inline::Styled {
                    style: InlineStyle::Strong,
                    ..
                }
            ))
            .count(),
        2
    );
    assert!(matches!(
        output.inlines.last(),
        Some(Inline::Text(text)) if text.value.ends_with("plus **")
    ));
}

#[test]
fn emphasis_parses_combinations_and_ignores_identifier_underscores() {
    let source = "_italic *bold `code`*_ and some_identifier";
    let output = parse(source, range(0, source.len()), InlineParseConfig::default());
    let Inline::Styled {
        style: InlineStyle::Emphasis,
        children,
        ..
    } = &output.inlines[0]
    else {
        panic!("expected emphasis");
    };
    assert!(matches!(
        children[1],
        Inline::Styled {
            style: InlineStyle::Strong,
            ..
        }
    ));
    assert!(matches!(
        output.inlines.last(),
        Some(Inline::Text(text)) if text.value.ends_with("some_identifier")
    ));
    assert!(output.problems.is_empty());
}

#[test]
fn inline_recovery_keeps_safe_spans_after_unclosed_emphasis() {
    let source = "_open then *strong* and `code`";
    let output = parse(source, range(0, source.len()), InlineParseConfig::default());
    assert!(output.inlines.iter().any(|inline| matches!(
        inline,
        Inline::Styled {
            style: InlineStyle::Strong,
            ..
        }
    )));
    assert!(output.inlines.iter().any(|inline| matches!(
        inline,
        Inline::Literal {
            kind: InlineLiteralKind::Monospace,
            ..
        }
    )));
    assert!(
        output
            .problems
            .iter()
            .any(|problem| problem.kind == InlineProblemKind::UnclosedEmphasis)
    );
}

#[test]
fn inline_recovery_reports_nesting_limit_and_keeps_source_text() {
    let source = "*outer _inner_*";
    let output = parse(
        source,
        range(0, source.len()),
        InlineParseConfig {
            max_depth: 1,
            ..InlineParseConfig::default()
        },
    );
    let Inline::Styled {
        style: InlineStyle::Strong,
        children,
        ..
    } = &output.inlines[0]
    else {
        panic!("expected outer strong");
    };
    assert!(matches!(
        &children[1],
        Inline::Text(text) if text.value == "_inner_"
    ));
    assert!(
        output
            .problems
            .iter()
            .any(|problem| problem.kind == InlineProblemKind::NestingLimitExceeded)
    );
}

#[test]
fn extended_quotes_and_passthroughs_build_typed_nodes() {
    let value = "#mark# H~2~O E=mc^2^ \"`double`\" '`single`' +*raw*+ pass:[_opaque_]";
    let parsed = parse(value, range(0, value.len()), InlineParseConfig::default());
    assert!(parsed.inlines.iter().any(|inline| matches!(
        inline,
        Inline::Styled {
            style: InlineStyle::Highlight,
            ..
        }
    )));
    assert!(parsed.inlines.iter().any(|inline| matches!(
        inline,
        Inline::Styled {
            style: InlineStyle::Subscript,
            ..
        }
    )));
    assert!(parsed.inlines.iter().any(|inline| matches!(
        inline,
        Inline::Styled {
            style: InlineStyle::Superscript,
            ..
        }
    )));
    assert!(parsed.inlines.iter().any(|inline| matches!(
        inline,
        Inline::Styled {
            style: InlineStyle::CurvedDoubleQuote,
            ..
        }
    )));
    assert!(parsed.inlines.iter().any(|inline| matches!(
        inline,
        Inline::Styled {
            style: InlineStyle::CurvedSingleQuote,
            ..
        }
    )));
    assert_eq!(
        parsed
            .inlines
            .iter()
            .filter(|inline| matches!(inline, Inline::Passthrough { .. }))
            .count(),
        2
    );
}
