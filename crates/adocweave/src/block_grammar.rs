//! Block grammar dispatch, isolated from block construction and lowering.

use crate::parser::{
    is_block_title, parse_block_attributes, parse_explicit_anchor, parse_math_attribute,
    parse_source_attribute, unsupported_reason,
};
use crate::source::TextRange;

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
