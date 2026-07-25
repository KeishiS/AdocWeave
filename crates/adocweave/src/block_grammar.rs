//! Block-level lexical recognition, isolated from construction and lowering.

use crate::block_model::{BlockMetadata, ElementAttribute, ExplicitAnchor, MetadataValue};
use crate::inline::MathLanguage;
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
    Blank,
    DocumentAttribute,
    Break,
    LiteralParagraph,
    Heading,
    List,
    Unsupported,
    Paragraph,
}

/// Classifies one source line without mutating parser state.
pub(crate) fn recognize_line(
    content: &str,
    next_content: Option<&str>,
    content_start: usize,
    full_range: TextRange,
    header_attributes_open: bool,
) -> LineRecognition {
    if parse_source_attribute(content).is_some() && next_content == Some("----") {
        LineRecognition::Source
    } else if content.starts_with("[source") && next_content == Some("----") {
        LineRecognition::InvalidSource
    } else if parse_math_attribute(content).is_some() && next_content == Some("++++") {
        LineRecognition::Math
    } else if crate::delimiter::spec(content).is_some() {
        LineRecognition::Delimited
    } else if parse_explicit_anchor(content, content_start, full_range)
        .filter(|_| content.starts_with("[["))
        .is_some()
    {
        LineRecognition::Anchor
    } else if is_block_title(content) {
        LineRecognition::BlockTitle
    } else if parse_block_attributes(content, content_start).is_some() {
        LineRecognition::BlockMetadata
    } else if content.trim_matches([' ', '\t']).is_empty() {
        LineRecognition::Blank
    } else if header_attributes_open
        && crate::attributes::parse_line(content, content_start, full_range).is_some()
    {
        LineRecognition::DocumentAttribute
    } else if matches!(content, "'''" | "<<<") {
        LineRecognition::Break
    } else if content.starts_with([' ', '\t']) {
        LineRecognition::LiteralParagraph
    } else if content.starts_with('=') {
        LineRecognition::Heading
    } else if crate::list_parser::marker(content).is_some() {
        LineRecognition::List
    } else if unsupported_reason(content).is_some() {
        LineRecognition::Unsupported
    } else {
        LineRecognition::Paragraph
    }
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
    let leading = language.len() - language.trim_start_matches([' ', '\t']).len();
    let trimmed = language.trim_matches([' ', '\t']);
    if trimmed.is_empty() {
        return Some(None);
    }
    if trimmed.contains([',', ']']) {
        return None;
    }
    let start = prefix_len + leading;
    Some(Some((start, start + trimmed.len())))
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
