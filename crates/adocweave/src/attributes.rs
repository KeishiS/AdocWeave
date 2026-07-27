//! Standard AsciiDoc document attributes and their source-ordered environment.

use std::collections::BTreeMap;

use crate::source::{TextRange, TextSize};
use crate::substitution::{
    AttributeExpansionError, AttributeExpansionLimits, expand_attribute_text,
};

/// The standard AsciiDoc operation represented by a document attribute line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentAttributeOperation {
    Set,
    Unset,
}

/// One source-preserving standard document-attribute occurrence.
///
/// This is a backend-independent syntax fact. Hosts may interpret attribute
/// names for their own metadata, but the core does not assign application-
/// specific meaning to them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentAttributeOccurrence {
    pub range: TextRange,
    pub name_range: TextRange,
    pub value_range: TextRange,
    pub name: String,
    pub raw_value: String,
    pub operation: DocumentAttributeOperation,
    pub valid: bool,
}

/// Stable identity of an attribute binding within one analysis.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AttributeBindingId(u32);

impl AttributeBindingId {
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Total ordering of attribute operations within one expanded source position.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AttributeEventId(u32);

impl AttributeEventId {
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

/// One point in expanded-source reading order.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct AttributePosition {
    offset: TextSize,
    event_id: AttributeEventId,
}

impl AttributePosition {
    pub const fn new(offset: TextSize, event_id: AttributeEventId) -> Self {
        Self { offset, event_id }
    }

    pub const fn offset(self) -> TextSize {
        self.offset
    }

    pub const fn event_id(self) -> AttributeEventId {
        self.event_id
    }
}

/// One effective set or unset operation in an [`AttributeEnvironment`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttributeBinding {
    id: AttributeBindingId,
    event_id: AttributeEventId,
    visible_at: TextSize,
    evaluation_at: TextSize,
    operation: DocumentAttributeOperation,
    raw_value: String,
    expansion_depth: u32,
    value: Result<Option<String>, AttributeExpansionError>,
    occurrence: DocumentAttributeOccurrence,
}

impl AttributeBinding {
    pub const fn id(&self) -> AttributeBindingId {
        self.id
    }

    pub const fn event_id(&self) -> AttributeEventId {
        self.event_id
    }

    pub const fn visible_at(&self) -> TextSize {
        self.visible_at
    }

    pub const fn visible_position(&self) -> AttributePosition {
        AttributePosition::new(self.visible_at, self.event_id)
    }

    pub const fn evaluation_at(&self) -> TextSize {
        self.evaluation_at
    }

    pub const fn operation(&self) -> DocumentAttributeOperation {
        self.operation
    }

    pub fn raw_value(&self) -> &str {
        &self.raw_value
    }

    pub const fn expansion_depth(&self) -> u32 {
        self.expansion_depth
    }

    pub fn value(&self) -> Result<Option<&str>, AttributeExpansionError> {
        self.value
            .as_ref()
            .map(|value| value.as_deref())
            .map_err(|error| *error)
    }

    pub const fn occurrence(&self) -> &DocumentAttributeOccurrence {
        &self.occurrence
    }
}

/// Value selected at a source position and the binding which selected it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedAttribute<'a> {
    pub value: Result<Option<&'a str>, AttributeExpansionError>,
    pub binding: &'a AttributeBinding,
}

/// Immutable, source-ordered document attribute state.
///
/// Bindings are stored once and indexed by name. Position lookups search only
/// the selected name's history, so storage is proportional to the number of
/// authored operations rather than the number of semantic nodes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttributeEnvironment {
    bindings: Vec<AttributeBinding>,
    histories: BTreeMap<String, Vec<usize>>,
    final_values: BTreeMap<String, String>,
    limits: AttributeExpansionLimits,
}

impl Default for AttributeEnvironment {
    fn default() -> Self {
        Self {
            bindings: Vec::new(),
            histories: BTreeMap::new(),
            final_values: BTreeMap::new(),
            limits: AttributeExpansionLimits {
                max_depth: u32::MAX,
                max_bytes: u32::MAX,
            },
        }
    }
}

impl AttributeEnvironment {
    pub(crate) fn build(
        occurrences: &[DocumentAttributeOccurrence],
        limits: AttributeExpansionLimits,
    ) -> Self {
        let mut environment = Self {
            limits,
            ..Self::default()
        };
        let mut current = BTreeMap::new();
        let mut current_depths = BTreeMap::new();
        let mut current_failures = BTreeMap::new();
        for (ordinal, occurrence) in occurrences.iter().enumerate() {
            if !occurrence.valid {
                continue;
            }
            let canonical_name = canonical_name(&occurrence.name);
            let id = AttributeBindingId(
                u32::try_from(environment.bindings.len()).expect("attribute limit fits u32"),
            );
            let event_id =
                AttributeEventId(u32::try_from(ordinal).expect("attribute limit fits u32"));
            let evaluated = match occurrence.operation {
                DocumentAttributeOperation::Set => evaluate_definition(
                    &canonical_name,
                    &occurrence.raw_value,
                    &current,
                    &current_depths,
                    &current_failures,
                    limits,
                )
                .map(|(value, depth)| (Some(value), depth)),
                DocumentAttributeOperation::Unset => Ok((None, 0)),
            };
            let expansion_depth = evaluated.as_ref().map_or(0, |(_, depth)| *depth);
            let value = evaluated.map(|(value, _)| value);
            match &value {
                Ok(Some(value)) => {
                    current.insert(canonical_name.clone(), value.clone());
                    current_depths.insert(canonical_name.clone(), expansion_depth);
                    current_failures.remove(&canonical_name);
                }
                Ok(None) => {
                    current.remove(&canonical_name);
                    current_depths.remove(&canonical_name);
                    current_failures.remove(&canonical_name);
                }
                Err(error) => {
                    current.remove(&canonical_name);
                    current_depths.remove(&canonical_name);
                    current_failures.insert(canonical_name.clone(), *error);
                }
            }
            let binding = AttributeBinding {
                id,
                event_id,
                visible_at: occurrence.range.end(),
                evaluation_at: occurrence.value_range.start(),
                operation: occurrence.operation,
                raw_value: occurrence.raw_value.clone(),
                expansion_depth,
                value,
                occurrence: occurrence.clone(),
            };
            let index = environment.bindings.len();
            environment
                .histories
                .entry(canonical_name)
                .or_default()
                .push(index);
            environment.bindings.push(binding);
        }
        environment.final_values = current;
        environment
    }

    pub fn bindings(&self) -> &[AttributeBinding] {
        &self.bindings
    }

    pub fn history(&self, name: &str) -> impl DoubleEndedIterator<Item = &AttributeBinding> {
        let name = canonical_name(name);
        self.histories
            .get(&name)
            .into_iter()
            .flatten()
            .map(|index| &self.bindings[*index])
    }

    pub fn resolve_at(&self, name: &str, offset: TextSize) -> Option<ResolvedAttribute<'_>> {
        self.resolve_at_event(
            name,
            AttributePosition::new(offset, AttributeEventId(u32::MAX)),
        )
    }

    pub fn resolve_at_event(
        &self,
        name: &str,
        position: AttributePosition,
    ) -> Option<ResolvedAttribute<'_>> {
        let name = canonical_name(name);
        let history = self.histories.get(&name)?;
        let visible =
            history.partition_point(|index| self.bindings[*index].visible_position() < position);
        let binding = &self.bindings[*history.get(visible.checked_sub(1)?)?];
        Some(ResolvedAttribute {
            value: binding.value(),
            binding,
        })
    }

    pub fn expand_at_event(
        &self,
        text: &str,
        position: AttributePosition,
    ) -> Result<String, AttributeExpansionError> {
        self.expand_with(text, |name| self.resolve_at_event(name, position))
    }

    pub fn expand_at(
        &self,
        text: &str,
        offset: TextSize,
    ) -> Result<String, AttributeExpansionError> {
        self.expand_with(text, |name| self.resolve_at(name, offset))
    }

    fn expand_with<'a>(
        &'a self,
        text: &str,
        mut resolve: impl FnMut(&str) -> Option<ResolvedAttribute<'a>>,
    ) -> Result<String, AttributeExpansionError> {
        expand_attribute_text(text, self.limits, |name| {
            let resolved = resolve(name).ok_or(AttributeExpansionError::Undefined)?;
            let value = resolved.value?.ok_or(AttributeExpansionError::Undefined)?;
            Ok((value.to_owned(), 0))
        })
        .map(|(value, _)| value)
    }

    pub fn final_values(&self) -> &BTreeMap<String, String> {
        &self.final_values
    }

    pub fn values_at(&self, offset: TextSize) -> BTreeMap<String, String> {
        self.histories
            .keys()
            .filter_map(|name| {
                self.resolve_at(name, offset)
                    .and_then(|resolved| resolved.value.ok().flatten())
                    .map(|value| (name.clone(), value.to_owned()))
            })
            .collect()
    }
}

fn canonical_name(name: &str) -> String {
    name.to_ascii_lowercase()
}

fn evaluate_definition(
    binding_name: &str,
    raw_value: &str,
    values: &BTreeMap<String, String>,
    depths: &BTreeMap<String, u32>,
    failures: &BTreeMap<String, AttributeExpansionError>,
    limits: AttributeExpansionLimits,
) -> Result<(String, u32), AttributeExpansionError> {
    expand_attribute_text(raw_value, limits, |name| {
        let name = canonical_name(name);
        let value = values.get(&name).ok_or_else(|| {
            if let Some(error) = failures.get(&name) {
                *error
            } else if name == binding_name {
                AttributeExpansionError::Cycle
            } else {
                AttributeExpansionError::Undefined
            }
        })?;
        Ok((
            value.clone(),
            depths.get(&name).copied().expect("value depth exists"),
        ))
    })
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
) -> Option<(DocumentAttributeOccurrence, Option<AttributeProblem>)> {
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

    let valid_name = name
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'));
    let valid_set_separator = after.is_empty() || after.starts_with([' ', '\t']);
    let (operation, problem) = if !valid_name {
        (
            DocumentAttributeOperation::Set,
            Some(AttributeProblem {
                kind: AttributeProblemKind::InvalidName,
                range: name_range,
                name: name.to_owned(),
            }),
        )
    } else if unset {
        (
            DocumentAttributeOperation::Unset,
            (!raw_value.is_empty()).then(|| AttributeProblem {
                kind: AttributeProblemKind::InvalidValue,
                range: value_range,
                name: name.to_owned(),
            }),
        )
    } else if !valid_set_separator {
        (
            DocumentAttributeOperation::Set,
            Some(AttributeProblem {
                kind: AttributeProblemKind::InvalidValue,
                range: value_range,
                name: name.to_owned(),
            }),
        )
    } else {
        (DocumentAttributeOperation::Set, None)
    };

    let valid = problem.is_none();
    Some((
        DocumentAttributeOccurrence {
            range: full_range,
            name_range,
            value_range,
            name: name.to_owned(),
            raw_value: raw_value.to_owned(),
            operation,
            valid,
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

#[cfg(test)]
mod tests {
    use super::{
        AttributeEnvironment, AttributeEventId, AttributePosition, DocumentAttributeOccurrence,
        DocumentAttributeOperation,
    };
    use crate::source::{TextRange, TextSize};
    use crate::substitution::AttributeExpansionLimits;

    fn occurrence(value: &str) -> DocumentAttributeOccurrence {
        DocumentAttributeOccurrence {
            range: range(0, 4),
            name_range: range(1, 2),
            value_range: range(3, 4),
            name: "Name".to_owned(),
            raw_value: value.to_owned(),
            operation: DocumentAttributeOperation::Set,
            valid: true,
        }
    }

    fn range(start: u32, end: u32) -> TextRange {
        TextRange::new(
            TextSize::new(start as usize).expect("start"),
            TextSize::new(end as usize).expect("end"),
        )
        .expect("range")
    }

    #[test]
    fn event_id_breaks_ties_at_the_same_expanded_offset() {
        let environment = AttributeEnvironment::build(
            &[occurrence("first"), occurrence("second")],
            AttributeExpansionLimits {
                max_depth: 8,
                max_bytes: 128,
            },
        );
        let at = |event| {
            environment
                .resolve_at_event(
                    "name",
                    AttributePosition::new(
                        TextSize::new(4).expect("offset"),
                        AttributeEventId::new(event),
                    ),
                )
                .map(|resolved| resolved.value)
        };

        assert_eq!(at(0), None);
        assert_eq!(at(1), Some(Ok(Some("first"))));
        assert_eq!(at(2), Some(Ok(Some("second"))));
        assert_eq!(
            environment.resolve_at("NAME", TextSize::new(4).expect("offset")),
            environment.resolve_at("name", TextSize::new(4).expect("offset"))
        );
    }
}
