//! Document attributes and note-profile metadata.

use crate::source::{TextRange, TextSize};

pub const NOTE_METADATA_NAMES: [&str; 6] = [
    "note-id",
    "creator-id",
    "created-at",
    "updated-at",
    "tags",
    "stem",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AttributeValue {
    String(String),
    Uuid(String),
    DateTime(String),
    Tags(Vec<String>),
    Stem(StemKind),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StemKind {
    LatexMath,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AttributeOperation {
    Set(AttributeValue),
    Unset,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentAttribute {
    pub range: TextRange,
    pub name_range: TextRange,
    pub value_range: TextRange,
    pub name: String,
    pub raw_value: String,
    pub operation: AttributeOperation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttributeProblemKind {
    InvalidName,
    InvalidValue,
}

#[derive(Clone, Debug, Eq, PartialEq)]
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
            AttributeOperation::Set(AttributeValue::String(raw_value.to_owned())),
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
        match typed_value(name, raw_value) {
            Some(value) => (AttributeOperation::Set(value), None),
            None => (
                AttributeOperation::Set(AttributeValue::String(raw_value.to_owned())),
                Some(AttributeProblem {
                    kind: AttributeProblemKind::InvalidValue,
                    range: value_range,
                    name: name.to_owned(),
                }),
            ),
        }
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

fn typed_value(name: &str, value: &str) -> Option<AttributeValue> {
    match name {
        "note-id" | "creator-id" => {
            is_uuid(value).then(|| AttributeValue::Uuid(value.to_ascii_lowercase()))
        }
        "created-at" | "updated-at" => {
            is_rfc3339_shape(value).then(|| AttributeValue::DateTime(value.to_owned()))
        }
        "tags" => {
            let tags = value
                .split(',')
                .map(str::trim)
                .filter(|tag| !tag.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>();
            (!tags.is_empty()).then_some(AttributeValue::Tags(tags))
        }
        "stem" => match value.to_ascii_lowercase().as_str() {
            "latexmath" | "latex" => Some(AttributeValue::Stem(StemKind::LatexMath)),
            _ => None,
        },
        _ => Some(AttributeValue::String(value.to_owned())),
    }
}

fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

fn is_rfc3339_shape(value: &str) -> bool {
    let bytes = value.as_bytes();
    value.len() >= 20
        && bytes.get(4) == Some(&b'-')
        && bytes.get(7) == Some(&b'-')
        && bytes.get(10) == Some(&b'T')
        && bytes.get(13) == Some(&b':')
        && bytes.get(16) == Some(&b':')
        && (value.ends_with('Z')
            || value
                .get(19..)
                .is_some_and(|suffix| suffix.contains('+') || suffix.contains('-')))
        && value
            .chars()
            .all(|character| character.is_ascii_digit() || "-T:+.Z".contains(character))
}

fn range(start: usize, end: usize) -> TextRange {
    TextRange::new(
        TextSize::new(start).expect("attribute offset fits"),
        TextSize::new(end).expect("attribute offset fits"),
    )
    .expect("attribute range is ordered")
}
