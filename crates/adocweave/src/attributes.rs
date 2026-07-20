//! Standard AsciiDoc document attributes.

use crate::source::{TextRange, TextSize};

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub enum AttributeOperation {
    Set,
    Unset,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct DocumentAttribute {
    pub range: TextRange,
    pub name_range: TextRange,
    pub value_range: TextRange,
    pub name: String,
    pub raw_value: String,
    pub operation: AttributeOperation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub enum AttributeProblemKind {
    InvalidName,
    InvalidValue,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct AttributeProblem {
    pub kind: AttributeProblemKind,
    pub range: TextRange,
    pub name: String,
}

pub(crate) fn parse_line(
    content: &str,
    absolute_start: usize,
    full_range: TextRange,
) -> Option<(DocumentAttribute, Option<AttributeProblem>)> {
    let inner = content.strip_prefix(':')?;
    let delimiter = inner.find(':')?;
    let raw_name = &inner[..delimiter];
    let after = &inner[delimiter + 1..];

    let (name, unset) = if let Some(name) = raw_name.strip_prefix('!') {
        (name, true)
    } else if let Some(name) = raw_name.strip_suffix('!') {
        (name, true)
    } else {
        (raw_name, false)
    };
    let name_offset = 1 + usize::from(raw_name.starts_with('!'));
    let name_range = range(
        absolute_start + name_offset,
        absolute_start + name_offset + name.len(),
    );
    let leading = after.len() - after.trim_start_matches([' ', '\t']).len();
    let raw_value = after.trim_matches([' ', '\t']);
    let value_start = absolute_start + 1 + delimiter + 1 + leading;
    let value_range = range(value_start, value_start + raw_value.len());

    let valid_name = !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'));
    let (operation, problem) = if !valid_name {
        (
            AttributeOperation::Set,
            Some(AttributeProblem {
                kind: AttributeProblemKind::InvalidName,
                range: name_range,
                name: name.to_owned(),
            }),
        )
    } else if unset {
        (
            AttributeOperation::Unset,
            (!raw_value.is_empty()).then(|| AttributeProblem {
                kind: AttributeProblemKind::InvalidValue,
                range: value_range,
                name: name.to_owned(),
            }),
        )
    } else {
        (AttributeOperation::Set, None)
    };

    Some((
        DocumentAttribute {
            range: full_range,
            name_range,
            value_range,
            name: name.to_owned(),
            raw_value: raw_value.to_owned(),
            operation,
        },
        problem,
    ))
}

fn range(start: usize, end: usize) -> TextRange {
    TextRange::new(
        TextSize::new(start).expect("attribute offset fits"),
        TextSize::new(end).expect("attribute offset fits"),
    )
    .expect("attribute range is ordered")
}
