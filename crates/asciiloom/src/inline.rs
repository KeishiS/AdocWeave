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
}

impl Inline {
    pub const fn range(&self) -> TextRange {
        match self {
            Self::Text(text) => text.range,
        }
    }
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
    if value.is_empty() {
        Vec::new()
    } else {
        vec![Inline::Text(InlineText {
            range,
            value: value.to_owned(),
        })]
    }
}

pub fn inline_at(inlines: &[Inline], offset: u32) -> Option<&Inline> {
    inlines.iter().find(|inline| {
        let range = inline.range();
        range.start().to_u32() <= offset && offset < range.end().to_u32()
    })
}

#[cfg(test)]
mod tests {
    use super::{Inline, InlineParseConfig, inline_at, parse_text};
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
        let Inline::Text(text) = &inlines[0];

        assert_eq!(text.value, "日本語 😀");
        assert_eq!(text.range, range(4, 18));
        assert_eq!(inline_at(&inlines, 6), Some(&inlines[0]));
        assert_eq!(inline_at(&inlines, 18), None);
    }

    #[test]
    fn inline_text_handles_empty_input() {
        assert!(parse_text("", range(0, 0), InlineParseConfig::default()).is_empty());
    }
}
