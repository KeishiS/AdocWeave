//! Public, source-preserving document-attribute facts.

use crate::source::TextRange;

/// The standard AsciiDoc operation represented by a document attribute line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentAttributeOperation {
    Set,
    Unset,
}

/// One document-attribute occurrence in source order.
///
/// This is a backend-independent syntax fact. Hosts may interpret attribute
/// names for their own metadata, but the core does not assign application-
/// specific meaning to them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentAttributeOccurrence {
    /// Complete attribute-line range, including delimiters and line ending.
    pub range: TextRange,
    /// Attribute-name range without the leading or trailing unset marker.
    pub name_range: TextRange,
    /// Trimmed value range. This is empty for an empty set or unset.
    pub value_range: TextRange,
    pub name: String,
    pub raw_value: String,
    pub operation: DocumentAttributeOperation,
}

impl DocumentAttributeOccurrence {
    pub(crate) fn from_internal(attribute: &crate::attributes::DocumentAttribute) -> Self {
        Self {
            range: attribute.range,
            name_range: attribute.name_range,
            value_range: attribute.value_range,
            name: attribute.name.clone(),
            raw_value: attribute.raw_value.clone(),
            operation: match attribute.operation {
                crate::attributes::AttributeOperation::Set => DocumentAttributeOperation::Set,
                crate::attributes::AttributeOperation::Unset => DocumentAttributeOperation::Unset,
            },
        }
    }
}
