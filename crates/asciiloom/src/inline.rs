//! Output-independent inline syntax.

use crate::source::{TextRange, TextSize};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InlineText {
    pub range: TextRange,
    pub value: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Inline {
    Text(InlineText),
    Literal {
        kind: InlineLiteralKind,
        range: TextRange,
        content_range: TextRange,
        value: String,
    },
    Styled {
        style: InlineStyle,
        range: TextRange,
        content_range: TextRange,
        children: Vec<Inline>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InlineLiteralKind {
    Monospace,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InlineStyle {
    Strong,
    Emphasis,
}

impl Inline {
    pub const fn range(&self) -> TextRange {
        match self {
            Self::Text(text) => text.range,
            Self::Literal { range, .. } | Self::Styled { range, .. } => *range,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InlineProblemKind {
    UnclosedMonospace,
    UnclosedStrong,
    UnclosedEmphasis,
    NestingLimitExceeded,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InlineProblem {
    pub kind: InlineProblemKind,
    pub range: TextRange,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InlineParseOutput {
    pub inlines: Vec<Inline>,
    pub problems: Vec<InlineProblem>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InlineParseConfig {
    pub max_depth: usize,
}

impl Default for InlineParseConfig {
    fn default() -> Self {
        Self { max_depth: 32 }
    }
}

pub fn parse_text(value: &str, range: TextRange, config: InlineParseConfig) -> Vec<Inline> {
    parse(value, range, config).inlines
}

pub fn parse(value: &str, range: TextRange, config: InlineParseConfig) -> InlineParseOutput {
    parse_segment(value, range, config, 0)
}

fn parse_segment(
    value: &str,
    range: TextRange,
    config: InlineParseConfig,
    depth: usize,
) -> InlineParseOutput {
    let mut output = InlineParseOutput::default();
    let mut cursor = 0;
    let mut plain_start = 0;

    while let Some((open, marker)) = next_opener(value, cursor) {
        let Some(close) = find_closer(value, open, marker) else {
            output.problems.push(InlineProblem {
                kind: match marker {
                    '`' => InlineProblemKind::UnclosedMonospace,
                    '*' => InlineProblemKind::UnclosedStrong,
                    '_' => InlineProblemKind::UnclosedEmphasis,
                    _ => unreachable!("only supported markers are returned"),
                },
                range: subrange(range, open, open + marker.len_utf8()),
            });
            cursor = open + marker.len_utf8();
            continue;
        };

        push_text(&mut output.inlines, value, range, plain_start, open);
        let node_range = subrange(range, open, close + marker.len_utf8());
        let content_range = subrange(range, open + marker.len_utf8(), close);
        match marker {
            '`' => output.inlines.push(Inline::Literal {
                kind: InlineLiteralKind::Monospace,
                range: node_range,
                content_range,
                value: value[open + 1..close].to_owned(),
            }),
            '*' | '_' if depth >= config.max_depth => {
                output.inlines.push(Inline::Text(InlineText {
                    range: node_range,
                    value: value[open..=close].to_owned(),
                }));
                output.problems.push(InlineProblem {
                    kind: InlineProblemKind::NestingLimitExceeded,
                    range: node_range,
                });
            }
            '*' => {
                let inner =
                    parse_segment(&value[open + 1..close], content_range, config, depth + 1);
                output.problems.extend(inner.problems);
                output.inlines.push(Inline::Styled {
                    style: InlineStyle::Strong,
                    range: node_range,
                    content_range,
                    children: inner.inlines,
                });
            }
            '_' => {
                let inner =
                    parse_segment(&value[open + 1..close], content_range, config, depth + 1);
                output.problems.extend(inner.problems);
                output.inlines.push(Inline::Styled {
                    style: InlineStyle::Emphasis,
                    range: node_range,
                    content_range,
                    children: inner.inlines,
                });
            }
            _ => unreachable!("only supported markers are returned"),
        }
        cursor = close + marker.len_utf8();
        plain_start = cursor;
    }

    push_text(&mut output.inlines, value, range, plain_start, value.len());
    output
}

fn next_opener(value: &str, cursor: usize) -> Option<(usize, char)> {
    value[cursor..]
        .char_indices()
        .map(|(offset, marker)| (cursor + offset, marker))
        .find(|(offset, marker)| {
            matches!(marker, '`' | '*' | '_') && is_open_boundary(value, *offset, *marker)
        })
}

fn find_closer(value: &str, open: usize, marker: char) -> Option<usize> {
    value[open + marker.len_utf8()..]
        .char_indices()
        .map(|(offset, candidate)| (open + marker.len_utf8() + offset, candidate))
        .find(|(offset, candidate)| {
            *candidate == marker && is_close_boundary(value, *offset, marker)
        })
        .map(|(offset, _)| offset)
}

fn is_open_boundary(value: &str, offset: usize, marker: char) -> bool {
    let previous = value[..offset].chars().next_back();
    let next = value[offset + marker.len_utf8()..].chars().next();
    next.is_some_and(|character| !character.is_whitespace() && character != marker)
        && previous.is_none_or(|character| !character.is_alphanumeric())
}

fn is_close_boundary(value: &str, offset: usize, marker: char) -> bool {
    let previous = value[..offset].chars().next_back();
    let next = value[offset + marker.len_utf8()..].chars().next();
    previous.is_some_and(|character| !character.is_whitespace() && character != marker)
        && next.is_none_or(|character| !character.is_alphanumeric())
}

fn push_text(inlines: &mut Vec<Inline>, value: &str, range: TextRange, start: usize, end: usize) {
    if start != end {
        inlines.push(Inline::Text(InlineText {
            range: subrange(range, start, end),
            value: value[start..end].to_owned(),
        }));
    }
}

fn subrange(parent: TextRange, start: usize, end: usize) -> TextRange {
    let base = parent.start().to_usize();
    TextRange::new(
        TextSize::new(base + start).expect("inline offset fits"),
        TextSize::new(base + end).expect("inline offset fits"),
    )
    .expect("inline range is ordered")
}

pub fn inline_at(inlines: &[Inline], offset: u32) -> Option<&Inline> {
    inlines.iter().find_map(|inline| {
        let range = inline.range();
        if range.start().to_u32() <= offset && offset < range.end().to_u32() {
            match inline {
                Inline::Styled { children, .. } => inline_at(children, offset).or(Some(inline)),
                _ => Some(inline),
            }
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{
        Inline, InlineLiteralKind, InlineParseConfig, InlineProblemKind, InlineStyle, inline_at,
        parse, parse_text,
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
    fn monospace_requires_constrained_boundaries() {
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
            InlineParseConfig { max_depth: 1 },
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
}
