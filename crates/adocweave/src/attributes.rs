//! Standard AsciiDoc document attributes and their source-ordered environment.

use std::collections::{BTreeMap, BTreeSet};

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

/// How one physical attribute-value line continues onto the next line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttributeValueContinuation {
    Soft,
    Hard,
}

/// The marker which continues an attribute value onto the next physical line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocumentAttributeContinuation {
    pub kind: AttributeValueContinuation,
    pub range: TextRange,
}

/// One physical line of a document attribute value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentAttributeValueLine {
    pub range: TextRange,
    pub indent_range: TextRange,
    pub content_range: TextRange,
    pub ending_range: TextRange,
    pub continuation: Option<DocumentAttributeContinuation>,
}

/// Source and semantic forms of one document attribute value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentAttributeValue {
    pub source_range: TextRange,
    pub source_text: String,
    pub folded_text: String,
    pub lines: Vec<DocumentAttributeValueLine>,
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
    pub name: String,
    pub value: DocumentAttributeValue,
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
    folded_value: String,
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

    pub fn source_text(&self) -> &str {
        &self.occurrence.value.source_text
    }

    pub fn folded_value(&self) -> &str {
        &self.folded_value
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

/// Mutable source-order state shared by preprocessing and semantic lowering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SequentialAttributeState {
    values: BTreeMap<String, String>,
    depths: BTreeMap<String, u32>,
    failures: BTreeMap<String, AttributeExpansionError>,
    locked: BTreeSet<String>,
    limits: AttributeExpansionLimits,
}

impl SequentialAttributeState {
    pub(crate) fn empty(limits: AttributeExpansionLimits) -> Self {
        Self {
            values: BTreeMap::new(),
            depths: BTreeMap::new(),
            failures: BTreeMap::new(),
            locked: BTreeSet::new(),
            limits,
        }
    }

    pub(crate) fn with_locked_values(
        values: &BTreeMap<String, String>,
        limits: AttributeExpansionLimits,
    ) -> Self {
        let values = values
            .iter()
            .map(|(name, value)| (canonical_name(name), value.clone()))
            .collect::<BTreeMap<_, _>>();
        Self {
            depths: values.keys().map(|name| (name.clone(), 0)).collect(),
            locked: values.keys().cloned().collect(),
            values,
            failures: BTreeMap::new(),
            limits,
        }
    }

    pub(crate) fn apply(
        &mut self,
        occurrence: &DocumentAttributeOccurrence,
    ) -> Result<Option<String>, AttributeExpansionError> {
        if !occurrence.valid {
            return Ok(None);
        }
        let name = canonical_name(&occurrence.name);
        if self.locked.contains(&name) {
            return Ok(self.values.get(&name).cloned());
        }
        let evaluated = match occurrence.operation {
            DocumentAttributeOperation::Set => evaluate_definition(
                &name,
                &occurrence.value.folded_text,
                &self.values,
                &self.depths,
                &self.failures,
                self.limits,
            )
            .map(|(value, depth)| (Some(value), depth)),
            DocumentAttributeOperation::Unset => Ok((None, 0)),
        };
        match &evaluated {
            Ok((Some(value), depth)) => {
                self.values.insert(name.clone(), value.clone());
                self.depths.insert(name.clone(), *depth);
                self.failures.remove(&name);
            }
            Ok((None, _)) => {
                self.values.remove(&name);
                self.depths.remove(&name);
                self.failures.remove(&name);
            }
            Err(error) => {
                self.values.remove(&name);
                self.depths.remove(&name);
                self.failures.insert(name, *error);
            }
        }
        evaluated.map(|(value, _)| value)
    }

    pub(crate) const fn values(&self) -> &BTreeMap<String, String> {
        &self.values
    }
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
        let mut state = SequentialAttributeState::empty(limits);
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
            let value = state.apply(occurrence);
            let expansion_depth = state.depths.get(&canonical_name).copied().unwrap_or(0);
            let binding = AttributeBinding {
                id,
                event_id,
                visible_at: occurrence.range.end(),
                evaluation_at: occurrence.value.source_range.start(),
                operation: occurrence.operation,
                folded_value: occurrence.value.folded_text.clone(),
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
        environment.final_values = state.values;
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
    let content_end = absolute_start + content.len();

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
            name: name.to_owned(),
            value: DocumentAttributeValue {
                source_range: value_range,
                source_text: raw_value.to_owned(),
                folded_text: raw_value.to_owned(),
                lines: vec![DocumentAttributeValueLine {
                    range: range(value_start, full_range.end().to_usize()),
                    indent_range: range(value_start, value_start),
                    content_range: value_range,
                    ending_range: range(content_end, full_range.end().to_usize()),
                    continuation: None,
                }],
            },
            operation,
            valid,
        },
        problem,
    ))
}

pub(crate) fn parse_lines(
    source_document: &crate::source_document::SourceDocument,
    line_index: usize,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<
    Option<(DocumentAttributeOccurrence, Option<AttributeProblem>, usize)>,
    crate::parser_support::ParseFailure,
> {
    let lines = source_document.lines();
    let Some(first_line) = lines.get(line_index).copied() else {
        return Ok(None);
    };
    let Some(first_content) = source_document.text(first_line.content_range()) else {
        return Ok(None);
    };
    let (mut occurrence, mut problem) = parse_line(
        first_content,
        first_line.content_range().start().to_usize(),
        first_line.full_range(),
    )
    .ok_or(crate::parser_support::ParseFailure::InternalInvariant)?;
    if occurrence.operation == DocumentAttributeOperation::Unset
        || continuation_start(first_content).is_none()
        || line_index + 1 == lines.len()
    {
        return Ok(Some((occurrence, problem, line_index)));
    }

    let parsed_value_start = occurrence.value.source_range.start().to_usize();
    let first_continuation =
        continuation_start(first_content).expect("the first line was checked for a continuation");
    let value_start =
        parsed_value_start.min(first_line.content_range().start().to_usize() + first_continuation);
    let mut value_lines = Vec::new();
    let mut folded = String::new();
    let mut last_line = line_index;
    let mut value_end = value_start;

    for index in line_index..lines.len() {
        if is_cancelled() {
            return Err(crate::parser_support::ParseFailure::Cancelled);
        }
        let line = lines[index];
        let content = source_document
            .text(line.content_range())
            .expect("source line range is valid");
        let content_start = line.content_range().start().to_usize();
        let indent_end = if index == line_index {
            parsed_value_start
        } else {
            content_start + content.len() - content.trim_start_matches([' ', '\t']).len()
        };
        let continuation = continuation_start(content).filter(|_| index + 1 < lines.len());
        let marker_start = continuation.map(|start| content_start + start);
        let segment_start = marker_start.map_or(indent_end, |start| indent_end.min(start));
        let untrimmed_end = marker_start.unwrap_or(content_start + content.len());
        let segment_source = &source_document.source()[segment_start..untrimmed_end];
        let segment_text = if continuation.is_some() {
            segment_source
        } else {
            segment_source.trim_end_matches([' ', '\t'])
        };
        let segment_end = segment_start + segment_text.len();
        let continuation_kind = continuation.map(|_| {
            if segment_text.ends_with(" +") {
                AttributeValueContinuation::Hard
            } else {
                AttributeValueContinuation::Soft
            }
        });
        let continuation_range =
            continuation.map(|start| range(content_start + start, content_start + content.len()));
        folded.push_str(segment_text);
        match continuation_kind {
            Some(AttributeValueContinuation::Soft) => folded.push(' '),
            Some(AttributeValueContinuation::Hard) => folded.push('\n'),
            None => {}
        }
        value_lines.push(DocumentAttributeValueLine {
            range: range(
                if index == line_index {
                    value_start
                } else {
                    content_start
                },
                line.full_range().end().to_usize(),
            ),
            indent_range: range(
                if index == line_index {
                    value_start
                } else {
                    content_start
                },
                segment_start,
            ),
            content_range: range(segment_start, segment_end),
            ending_range: line.ending_range(),
            continuation: continuation_kind
                .zip(continuation_range)
                .map(|(kind, range)| DocumentAttributeContinuation { kind, range }),
        });
        last_line = index;
        value_end = segment_end;
        if continuation.is_none() {
            break;
        }
    }

    occurrence.range = range(
        first_line.full_range().start().to_usize(),
        lines[last_line].full_range().end().to_usize(),
    );
    occurrence.value = DocumentAttributeValue {
        source_range: range(value_start, value_end),
        source_text: source_document.source()[value_start..value_end].to_owned(),
        folded_text: folded,
        lines: value_lines,
    };
    if let Some(problem) = &mut problem
        && problem.kind == AttributeProblemKind::InvalidValue
    {
        problem.range = occurrence.value.source_range;
    }
    Ok(Some((occurrence, problem, last_line)))
}

fn continuation_start(content: &str) -> Option<usize> {
    content.ends_with(" \\").then(|| content.len() - 2)
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
            name: "Name".to_owned(),
            value: super::DocumentAttributeValue {
                source_range: range(3, 4),
                source_text: value.to_owned(),
                folded_text: value.to_owned(),
                lines: vec![super::DocumentAttributeValueLine {
                    range: range(3, 4),
                    indent_range: range(3, 3),
                    content_range: range(3, 4),
                    ending_range: range(4, 4),
                    continuation: None,
                }],
            },
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
