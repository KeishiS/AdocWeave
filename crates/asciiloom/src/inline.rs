//! Output-independent inline syntax.

use crate::source::TextRange;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InlineText {
    pub range: TextRange,
    pub value: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Inline {
    Text(InlineText),
    Monospace {
        range: TextRange,
        content_range: TextRange,
        value: String,
    },
}

impl Inline {
    pub const fn range(&self) -> TextRange {
        match self {
            Self::Text(text) => text.range,
            Self::Monospace { range, .. } => *range,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InlineProblemKind {
    UnclosedMonospace,
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

pub fn parse_text(value: &str, range: TextRange, _config: InlineParseConfig) -> Vec<Inline> {
    parse(value, range, InlineParseConfig::default()).inlines
}

pub fn parse(value: &str, range: TextRange, _config: InlineParseConfig) -> InlineParseOutput {
    let mut output = InlineParseOutput::default();
    let mut cursor = 0;

    while let Some(relative_open) = value[cursor..].find('`') {
        let open = cursor + relative_open;
        if !is_open_boundary(value, open) {
            cursor = open + 1;
            continue;
        }
        let search_start = open + 1;
        let close = value[search_start..]
            .match_indices('`')
            .map(|(offset, _)| search_start + offset)
            .find(|close| is_close_boundary(value, *close));
        let Some(close) = close else {
            push_text(&mut output.inlines, value, range, cursor, value.len());
            output.problems.push(InlineProblem {
                kind: InlineProblemKind::UnclosedMonospace,
                range: subrange(range, open, open + 1),
            });
            return output;
        };

        push_text(&mut output.inlines, value, range, cursor, open);
        output.inlines.push(Inline::Monospace {
            range: subrange(range, open, close + 1),
            content_range: subrange(range, open + 1, close),
            value: value[open + 1..close].to_owned(),
        });
        cursor = close + 1;
    }

    push_text(&mut output.inlines, value, range, cursor, value.len());
    output
}

fn is_open_boundary(value: &str, offset: usize) -> bool {
    let previous = value[..offset].chars().next_back();
    let next = value[offset + 1..].chars().next();
    next.is_some_and(|character| !character.is_whitespace() && character != '`')
        && previous.is_none_or(|character| !character.is_alphanumeric())
}

fn is_close_boundary(value: &str, offset: usize) -> bool {
    let previous = value[..offset].chars().next_back();
    let next = value[offset + 1..].chars().next();
    previous.is_some_and(|character| !character.is_whitespace() && character != '`')
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
        crate::source::TextSize::new(base + start).expect("inline offset fits"),
        crate::source::TextSize::new(base + end).expect("inline offset fits"),
    )
    .expect("inline range is ordered")
}

pub fn inline_at(inlines: &[Inline], offset: u32) -> Option<&Inline> {
    inlines.iter().find(|inline| {
        let range = inline.range();
        range.start().to_u32() <= offset && offset < range.end().to_u32()
    })
}

#[cfg(test)]
mod tests {
    use super::{Inline, InlineParseConfig, InlineProblemKind, inline_at, parse, parse_text};
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
            range(10, 29),
            InlineParseConfig::default(),
        );

        assert_eq!(output.inlines.len(), 4);
        assert!(matches!(
            &output.inlines[1],
            Inline::Monospace { value, .. } if value == "one"
        ));
        assert!(matches!(
            &output.inlines[3],
            Inline::Monospace { value, .. } if value == "二"
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
}
