//! Standard AsciiDoc document attributes and their source-ordered environment.

use std::collections::BTreeMap;

use crate::source::{TextRange, TextSize};
use crate::substitution::{AttributeEvaluator, AttributeExpansionError, AttributeExpansionLimits};

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
    pub const fn get(self) -> u32 {
        self.0
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
            let id = AttributeBindingId(
                u32::try_from(environment.bindings.len()).expect("attribute limit fits u32"),
            );
            let event_id =
                AttributeEventId(u32::try_from(ordinal).expect("attribute limit fits u32"));
            let value = match occurrence.operation {
                DocumentAttributeOperation::Set => definition_depth(
                    &occurrence.name,
                    &occurrence.raw_value,
                    &current_depths,
                    &current_failures,
                    limits.max_depth,
                )
                .and_then(|_| {
                    AttributeEvaluator::new(&current, limits).expand_text(&occurrence.raw_value)
                })
                .map(Some),
                DocumentAttributeOperation::Unset => Ok(None),
            };
            let expansion_depth = definition_depth(
                &occurrence.name,
                &occurrence.raw_value,
                &current_depths,
                &current_failures,
                limits.max_depth,
            )
            .unwrap_or(0);
            match &value {
                Ok(Some(value)) => {
                    current.insert(occurrence.name.clone(), value.clone());
                    current_depths.insert(occurrence.name.clone(), expansion_depth);
                    current_failures.remove(&occurrence.name);
                }
                Ok(None) => {
                    current.remove(&occurrence.name);
                    current_depths.remove(&occurrence.name);
                    current_failures.remove(&occurrence.name);
                }
                Err(error) => {
                    current.remove(&occurrence.name);
                    current_depths.remove(&occurrence.name);
                    current_failures.insert(occurrence.name.clone(), *error);
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
                .entry(occurrence.name.clone())
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
        self.histories
            .get(name)
            .into_iter()
            .flatten()
            .map(|index| &self.bindings[*index])
    }

    pub fn resolve_at(&self, name: &str, offset: TextSize) -> Option<ResolvedAttribute<'_>> {
        let history = self.histories.get(name)?;
        let visible = history.partition_point(|index| self.bindings[*index].visible_at <= offset);
        let binding = &self.bindings[*history.get(visible.checked_sub(1)?)?];
        Some(ResolvedAttribute {
            value: binding.value(),
            binding,
        })
    }

    pub fn expand_at(
        &self,
        text: &str,
        offset: TextSize,
    ) -> Result<String, AttributeExpansionError> {
        let mut output = String::new();
        let mut cursor = 0;
        while cursor < text.len() {
            let rest = &text[cursor..];
            if rest.starts_with("\\{") {
                output.push('{');
                cursor += 2;
            } else if rest.starts_with('{') {
                let Some(close) = rest.find('}') else {
                    output.push_str(rest);
                    break;
                };
                let name = &rest[1..close];
                if name.is_empty() {
                    output.push_str("{}");
                } else {
                    let resolved = self
                        .resolve_at(name, offset)
                        .ok_or(AttributeExpansionError::Undefined)?;
                    let value = resolved.value?.ok_or(AttributeExpansionError::Undefined)?;
                    output.push_str(value);
                }
                cursor += close + 1;
            } else {
                let character = rest.chars().next().expect("non-empty remainder");
                output.push(character);
                cursor += character.len_utf8();
            }
            if output.len() > self.limits.max_bytes as usize {
                return Err(AttributeExpansionError::SizeLimitExceeded);
            }
        }
        Ok(output)
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

fn definition_depth(
    binding_name: &str,
    raw_value: &str,
    depths: &BTreeMap<String, u32>,
    failures: &BTreeMap<String, AttributeExpansionError>,
    max_depth: u32,
) -> Result<u32, AttributeExpansionError> {
    let mut depth = 0_u32;
    let mut cursor = 0;
    while cursor < raw_value.len() {
        let rest = &raw_value[cursor..];
        if rest.starts_with("\\{") {
            cursor += 2;
        } else if rest.starts_with('{') {
            let Some(close) = rest.find('}') else {
                break;
            };
            let name = &rest[1..close];
            if !name.is_empty() {
                let referenced = depths.get(name).copied().ok_or_else(|| {
                    if let Some(error) = failures.get(name) {
                        *error
                    } else if name == binding_name {
                        AttributeExpansionError::Cycle
                    } else {
                        AttributeExpansionError::Undefined
                    }
                })?;
                depth = depth.max(referenced.saturating_add(1));
                if depth > max_depth {
                    return Err(AttributeExpansionError::DepthLimitExceeded);
                }
            }
            cursor += close + 1;
        } else {
            let character = rest.chars().next().expect("non-empty remainder");
            cursor += character.len_utf8();
        }
    }
    Ok(depth)
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
