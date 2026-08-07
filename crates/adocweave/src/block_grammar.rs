//! Block-level lexical recognition, isolated from construction and lowering.

use crate::block_model::{BlockMetadata, ElementAttribute, ExplicitAnchor, MetadataValue};
use crate::inline_model::MathLanguage;
use crate::source::{TextRange, TextSize};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LineRecognition {
    Source,
    InvalidSource,
    Math,
    Delimited,
    Anchor,
    BlockTitle,
    BlockMetadata,
    Comment,
    Blank,
    DocumentAttribute,
    Break,
    LiteralParagraph,
    Heading,
    List,
    PreprocessorDirective,
    Unsupported,
    Paragraph,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BlockRecognizer {
    Source,
    InvalidSource,
    Math,
    Delimited,
    Anchor,
    BlockTitle,
    BlockMetadata,
    Comment,
    Blank,
    DocumentAttribute,
    Break,
    LiteralParagraph,
    Heading,
    List,
    PreprocessorDirective,
    Unsupported,
}

const BLOCK_RECOGNIZER_PRIORITY: &[BlockRecognizer] = &[
    BlockRecognizer::Source,
    BlockRecognizer::InvalidSource,
    BlockRecognizer::Math,
    BlockRecognizer::Delimited,
    BlockRecognizer::Anchor,
    BlockRecognizer::BlockTitle,
    BlockRecognizer::BlockMetadata,
    BlockRecognizer::Comment,
    BlockRecognizer::Blank,
    BlockRecognizer::DocumentAttribute,
    BlockRecognizer::Break,
    BlockRecognizer::LiteralParagraph,
    BlockRecognizer::Heading,
    BlockRecognizer::List,
    // After `LiteralParagraph`, so an indented directive stays literal text:
    // AsciiDoc only reads a directive that starts at the first column.
    BlockRecognizer::PreprocessorDirective,
    BlockRecognizer::Unsupported,
];

struct RecognitionInput<'a> {
    content: &'a str,
    next_content: Option<&'a str>,
    content_start: usize,
    full_range: TextRange,
    document_attribute_position: bool,
}

impl BlockRecognizer {
    fn recognize(self, input: &RecognitionInput<'_>) -> Option<LineRecognition> {
        let content = input.content;
        match self {
            Self::Source => (parse_source_attribute(content).is_some()
                && input.next_content == Some("----"))
            .then_some(LineRecognition::Source),
            Self::InvalidSource => (content.starts_with("[source")
                && input.next_content == Some("----"))
            .then_some(LineRecognition::InvalidSource),
            Self::Math => (parse_math_attribute(content).is_some()
                && input.next_content == Some("++++"))
            .then_some(LineRecognition::Math),
            Self::Delimited => crate::delimiter::spec(content).map(|_| LineRecognition::Delimited),
            Self::Anchor => parse_explicit_anchor(content, input.content_start, input.full_range)
                .filter(|_| content.starts_with("[["))
                .map(|_| LineRecognition::Anchor),
            Self::BlockTitle => is_block_title(content).then_some(LineRecognition::BlockTitle),
            Self::BlockMetadata => parse_block_attributes(content, input.content_start)
                .map(|_| LineRecognition::BlockMetadata),
            Self::Comment => content
                .starts_with("//")
                .then_some(LineRecognition::Comment),
            Self::Blank => content
                .trim_matches([' ', '\t'])
                .is_empty()
                .then_some(LineRecognition::Blank),
            Self::DocumentAttribute => (input.document_attribute_position
                && crate::attributes::parse_line(content, input.content_start, input.full_range)
                    .is_some())
            .then_some(LineRecognition::DocumentAttribute),
            Self::Break => matches!(content, "'''" | "<<<").then_some(LineRecognition::Break),
            Self::LiteralParagraph => content
                .starts_with([' ', '\t'])
                .then_some(LineRecognition::LiteralParagraph),
            Self::Heading => content.starts_with('=').then_some(LineRecognition::Heading),
            Self::List => crate::list_parser::marker(content).map(|_| LineRecognition::List),
            Self::PreprocessorDirective => crate::preprocessor::classify_line(content)
                .map(|_| LineRecognition::PreprocessorDirective),
            Self::Unsupported => unsupported_reason(content).map(|_| LineRecognition::Unsupported),
        }
    }
}

/// Classifies one source line without mutating parser state.
pub(crate) fn recognize_line(
    content: &str,
    next_content: Option<&str>,
    content_start: usize,
    full_range: TextRange,
    document_attribute_position: bool,
) -> LineRecognition {
    let input = RecognitionInput {
        content,
        next_content,
        content_start,
        full_range,
        document_attribute_position,
    };
    BLOCK_RECOGNIZER_PRIORITY
        .iter()
        .find_map(|recognizer| recognizer.recognize(&input))
        .unwrap_or(LineRecognition::Paragraph)
}

pub(crate) fn parse_explicit_anchor(
    content: &str,
    absolute_start: usize,
    full_range: TextRange,
) -> Option<ExplicitAnchor> {
    let (inner, prefix_len) = if let Some(inner) = content
        .strip_prefix("[[")
        .and_then(|value| value.strip_suffix("]]"))
    {
        (inner, 2)
    } else {
        let inner = content
            .strip_prefix("[#")
            .and_then(|value| value.strip_suffix(']'))?;
        (inner, 2)
    };
    let (id, label) = inner
        .split_once(',')
        .map_or((inner, None), |(id, label)| (id, Some(label)));
    let id_range = text_range(
        absolute_start + prefix_len,
        absolute_start + prefix_len + id.len(),
    )?;
    let label_range = match label {
        Some(label) => Some(text_range(
            absolute_start + prefix_len + id.len() + 1,
            absolute_start + prefix_len + id.len() + 1 + label.len(),
        )?),
        None => None,
    };
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

pub(crate) fn is_block_title(content: &str) -> bool {
    content
        .strip_prefix('.')
        .is_some_and(|value| !value.is_empty() && !value.starts_with([' ', '\t', '.']))
}

pub(crate) fn parse_block_attributes(content: &str, base: usize) -> Option<BlockMetadata> {
    let inner = content.strip_prefix('[')?.strip_suffix(']')?;
    if inner.starts_with('[') || inner.ends_with(']') {
        return None;
    }
    let mut metadata = BlockMetadata::default();
    let mut field_start = 0;
    let mut quoted = false;
    for field_end in inner
        .char_indices()
        .filter_map(|(index, character)| {
            if character == '"' {
                quoted = !quoted;
            }
            (character == ',' && !quoted).then_some(index)
        })
        .chain(std::iter::once(inner.len()))
    {
        let raw = &inner[field_start..field_end];
        let leading = raw.len() - raw.trim_start().len();
        let value = raw.trim();
        let absolute_start = base + 1 + field_start + leading;
        let range = TextRange::new(
            TextSize::new(absolute_start).ok()?,
            TextSize::new(absolute_start + value.len()).ok()?,
        )
        .ok()?;
        if !value.is_empty() {
            parse_element_attribute(value, range, &mut metadata);
        }
        field_start = field_end.saturating_add(1);
    }
    Some(metadata)
}

fn parse_element_attribute(value: &str, range: TextRange, metadata: &mut BlockMetadata) {
    if let Some((name, raw_value)) = value.split_once('=') {
        let name = name.trim();
        let raw_value = raw_value.trim();
        metadata.attributes.push(ElementAttribute {
            name: (!name.is_empty()).then(|| name.to_owned()),
            value: unquote(raw_value).to_owned(),
            range,
        });
        return;
    }

    let mut shorthand = value;
    let mut consumed_shorthand = false;
    while let Some(marker) = shorthand
        .chars()
        .next()
        .filter(|value| matches!(value, '#' | '.' | '%'))
    {
        let tail = &shorthand[marker.len_utf8()..];
        let end = tail.find(['#', '.', '%']).unwrap_or(tail.len());
        let item = &tail[..end];
        if item.is_empty() {
            break;
        }
        let offset = value.len() - shorthand.len() + marker.len_utf8();
        let item_range = TextRange::new(
            TextSize::new(range.start().to_usize() + offset).expect("attribute offset is bounded"),
            TextSize::new(range.start().to_usize() + offset + item.len())
                .expect("attribute offset is bounded"),
        )
        .expect("ordered shorthand range");
        let item = MetadataValue {
            value: item.to_owned(),
            range: item_range,
        };
        match marker {
            '#' => metadata.id = Some(item),
            '.' => metadata.roles.push(item),
            '%' => metadata.options.push(item),
            _ => unreachable!(),
        }
        consumed_shorthand = true;
        shorthand = &tail[end..];
    }
    if !consumed_shorthand || !shorthand.is_empty() {
        metadata.attributes.push(ElementAttribute {
            name: None,
            value: unquote(value).to_owned(),
            range,
        });
    }
}

fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(value)
}

pub(crate) fn parse_math_attribute(text: &str) -> Option<MathLanguage> {
    match text {
        "[stem]" | "[latexmath]" => Some(MathLanguage::Latex),
        _ => None,
    }
}

pub(crate) fn parse_source_attribute(text: &str) -> Option<Option<(usize, usize)>> {
    let (language, prefix_len) = if let Some(inner) = text.strip_prefix("[source") {
        let inner = inner.strip_suffix(']')?;
        if inner.is_empty() {
            return Some(None);
        }
        (inner.strip_prefix(',')?, "[source,".len())
    } else {
        let inner = text.strip_prefix('[')?.strip_suffix(']')?;
        (inner.strip_prefix(',')?, "[,".len())
    };
    let (language, trailing) = language
        .split_once(',')
        .map_or((language, None), |(value, trailing)| {
            (value, Some(trailing))
        });
    if trailing.is_some_and(|trailing| {
        trailing.split(',').any(|value| {
            let value = value.trim();
            value != "linenums"
                && value != "%linenums"
                && !value.starts_with("start=")
                && value != "options=linenums"
        })
    }) {
        return None;
    }
    let leading = language.len() - language.trim_start_matches([' ', '\t']).len();
    let trimmed = language.trim_matches([' ', '\t']);
    if trimmed.is_empty() {
        return Some(None);
    }
    if trimmed.contains(']') {
        return None;
    }
    let start = prefix_len + leading;
    Some(Some((start, start + trimmed.len())))
}

/// Reason recorded on an [`crate::block_model::Unsupported`] block built from a
/// preprocessor directive that this analysis did not preprocess.
pub(crate) const CONDITIONAL_DIRECTIVE_REASON: &str = "conditional directive was not preprocessed";
pub(crate) const INCLUDE_DIRECTIVE_REASON: &str = "include directive was not preprocessed";

pub(crate) const fn directive_reason(line: crate::preprocessor::DirectiveLine) -> &'static str {
    match line {
        crate::preprocessor::DirectiveLine::Conditional => CONDITIONAL_DIRECTIVE_REASON,
        crate::preprocessor::DirectiveLine::Include => INCLUDE_DIRECTIVE_REASON,
    }
}

pub(crate) fn unsupported_reason(content: &str) -> Option<&'static str> {
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

pub(crate) fn trailing_whitespace_is_structural(content: &str) -> bool {
    let trimmed = content.trim_end_matches([' ', '\t']);
    trimmed != content
        && (crate::delimiter::spec(trimmed).is_some()
            || parse_block_attributes(trimmed, 0).is_some()
            || parse_source_attribute(trimmed).is_some()
            || parse_math_attribute(trimmed).is_some()
            || parse_explicit_anchor(
                trimmed,
                0,
                text_range(0, trimmed.len()).expect("short fixture range"),
            )
            .is_some())
}

pub(crate) fn valid_anchor_id(id: &str) -> bool {
    !id.is_empty()
        && id.chars().all(|character| {
            !character.is_control()
                && !character.is_whitespace()
                && !matches!(
                    character,
                    '[' | ']' | '<' | '>' | ',' | '#' | '"' | '\'' | '&' | '=' | '(' | ')'
                )
        })
}

fn text_range(start: usize, end: usize) -> Option<TextRange> {
    TextRange::new(TextSize::new(start).ok()?, TextSize::new(end).ok()?).ok()
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
    use super::*;

    fn range(content: &str) -> TextRange {
        TextRange::new(
            TextSize::ZERO,
            TextSize::new(content.len()).expect("test range"),
        )
        .expect("ordered test range")
    }

    fn recognize(
        content: &str,
        next_content: Option<&str>,
        document_attribute_position: bool,
    ) -> LineRecognition {
        recognize_line(
            content,
            next_content,
            0,
            range(content),
            document_attribute_position,
        )
    }

    #[test]
    fn block_recognizer_priority_is_explicit_complete_and_unique() {
        assert_eq!(
            BLOCK_RECOGNIZER_PRIORITY,
            &[
                BlockRecognizer::Source,
                BlockRecognizer::InvalidSource,
                BlockRecognizer::Math,
                BlockRecognizer::Delimited,
                BlockRecognizer::Anchor,
                BlockRecognizer::BlockTitle,
                BlockRecognizer::BlockMetadata,
                BlockRecognizer::Comment,
                BlockRecognizer::Blank,
                BlockRecognizer::DocumentAttribute,
                BlockRecognizer::Break,
                BlockRecognizer::LiteralParagraph,
                BlockRecognizer::Heading,
                BlockRecognizer::List,
                BlockRecognizer::PreprocessorDirective,
                BlockRecognizer::Unsupported,
            ]
        );
        let mut unique = BLOCK_RECOGNIZER_PRIORITY.to_vec();
        unique.sort_by_key(|recognizer| *recognizer as u8);
        unique.dedup();
        assert_eq!(unique.len(), BLOCK_RECOGNIZER_PRIORITY.len());
    }

    #[test]
    fn overlapping_forms_follow_the_static_priority() {
        assert_eq!(
            recognize("[source,rust]", Some("----"), false),
            LineRecognition::Source
        );
        assert_eq!(
            recognize("[source,rust,unknown]", Some("----"), false),
            LineRecognition::InvalidSource
        );
        assert_eq!(
            recognize("[stem]", Some("++++"), false),
            LineRecognition::Math
        );
        assert_eq!(recognize("----", None, false), LineRecognition::Delimited);
        assert_eq!(recognize("   ", None, false), LineRecognition::Blank);
        assert_eq!(
            recognize(" indented", None, false),
            LineRecognition::LiteralParagraph
        );
        assert_eq!(
            recognize(":name: value", None, true),
            LineRecognition::DocumentAttribute
        );
        assert_eq!(
            recognize(":name: value", None, false),
            LineRecognition::Paragraph
        );
    }

    #[test]
    fn preprocessor_directives_are_recognized_before_the_paragraph_fallback() {
        for line in [
            "ifdef::web[]",
            "ifdef::web[inline]",
            "ifndef::print[]",
            "ifeval::[\"{lang}\" == \"rust\"]",
            "endif::[]",
            "include::part.adoc[]",
        ] {
            assert_eq!(
                recognize(line, None, false),
                LineRecognition::PreprocessorDirective,
                "{line}"
            );
        }
    }

    #[test]
    fn escaped_and_indented_directives_stay_text() {
        // An escaped directive is written to be read, and AsciiDoc only reads a
        // directive that starts at the first column. Neither is a directive, so
        // neither may claim the reader's attention with a directive diagnostic.
        assert_eq!(
            recognize("\\ifdef::web[]", None, false),
            LineRecognition::Paragraph
        );
        assert_eq!(
            recognize("  ifdef::web[]", None, false),
            LineRecognition::LiteralParagraph
        );
        assert_eq!(
            recognize("ifdef::web", None, false),
            LineRecognition::Paragraph
        );
    }

    #[test]
    fn directive_reasons_are_distinct() {
        assert_eq!(
            directive_reason(crate::preprocessor::DirectiveLine::Conditional),
            CONDITIONAL_DIRECTIVE_REASON
        );
        assert_eq!(
            directive_reason(crate::preprocessor::DirectiveLine::Include),
            INCLUDE_DIRECTIVE_REASON
        );
        assert_ne!(CONDITIONAL_DIRECTIVE_REASON, INCLUDE_DIRECTIVE_REASON);
    }
}
