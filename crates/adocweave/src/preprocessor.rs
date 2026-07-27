//! Pure preprocessing over caller-provided resource snapshots.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;

use crate::core::{Analysis, Engine, ParseError, SourceId};
use crate::diagnostic::{Diagnostic, RelatedInformation, TextEdit};
use crate::document::DocumentSymbol;
use crate::inline::Reference;
use crate::resource::ResourceReference;
use crate::source::PositionError;
use crate::source::{TextRange, TextSize};
use crate::substitution::AttributeExpansionLimits;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SafeMode {
    Unsafe,
    Server,
    Safe,
    #[default]
    Secure,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceDocument {
    pub source_id: SourceId,
    pub source: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResourceSnapshot {
    resources: BTreeMap<String, ResourceDocument>,
}

impl ResourceSnapshot {
    pub fn insert(&mut self, target: impl Into<String>, document: ResourceDocument) {
        self.resources.insert(target.into(), document);
    }

    pub fn get(&self, target: &str) -> Option<&ResourceDocument> {
        self.resources.get(target)
    }
}

impl FromIterator<(String, ResourceDocument)> for ResourceSnapshot {
    fn from_iter<T: IntoIterator<Item = (String, ResourceDocument)>>(resources: T) -> Self {
        Self {
            resources: resources.into_iter().collect(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreprocessOptions {
    pub source_id: Option<SourceId>,
    pub base_uri: Option<String>,
    pub safe_mode: SafeMode,
    pub allowed_schemes: BTreeSet<String>,
    pub attributes: BTreeMap<String, String>,
    /// Expands include directives only from the caller-provided snapshot.
    pub enable_includes: bool,
    pub max_include_depth: u32,
    pub max_includes: u32,
    pub max_total_bytes: u32,
    pub max_expanded_nodes: u32,
    pub max_source_map_segments: u32,
}

impl Default for PreprocessOptions {
    fn default() -> Self {
        Self {
            source_id: None,
            base_uri: None,
            safe_mode: SafeMode::Secure,
            allowed_schemes: BTreeSet::new(),
            attributes: BTreeMap::new(),
            enable_includes: true,
            max_include_depth: 16,
            max_includes: 10_000,
            max_total_bytes: 50 * 1024 * 1024,
            max_expanded_nodes: 1_000_000,
            max_source_map_segments: 1_000_000,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DirectiveKind {
    Include,
    Ifdef,
    Ifndef,
    Ifeval,
    Endif,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Directive {
    pub kind: DirectiveKind,
    pub source_id: Option<SourceId>,
    pub range: TextRange,
    /// Source-relative include target after attribute expansion.
    pub authored_target: Option<String>,
    /// Whether a missing include resource is explicitly optional.
    pub optional: bool,
    pub target: String,
    pub target_range: TextRange,
    /// Definition target for an include; absent for conditionals.
    pub resource_source_id: Option<SourceId>,
}

impl Directive {
    pub fn local_target(&self) -> Option<crate::local_target::LocalTargetReference> {
        if self.kind != DirectiveKind::Include {
            return None;
        }
        crate::local_target::LocalTargetReference::from_include(
            self.range,
            self.target_range,
            self.authored_target.as_deref().unwrap_or(&self.target),
        )
    }
}

/// A non-fatal preprocessing event with a stable source range.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreprocessNotice {
    pub kind: PreprocessNoticeKind,
    pub source_id: Option<SourceId>,
    pub range: TextRange,
    pub target: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreprocessNoticeKind {
    OptionalResourceMissing,
}

impl PreprocessNoticeKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OptionalResourceMissing => "optional-resource-missing",
        }
    }
}

impl IncludeRequest {
    pub fn local_target(&self) -> Option<crate::local_target::LocalTargetReference> {
        crate::local_target::LocalTargetReference::from_include(
            self.range,
            self.target_range,
            &self.target,
        )
    }

    pub fn is_optional(&self) -> bool {
        parse_attributes(&self.attributes)
            .is_ok_and(|attributes| attributes.contains_key("optional"))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceOrigin {
    pub source_id: Option<SourceId>,
    pub range: OriginRange,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExpandedRange(TextRange);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExpandedOffset(TextSize);

impl ExpandedOffset {
    pub const fn new(offset: TextSize) -> Self {
        Self(offset)
    }

    pub const fn text_size(self) -> TextSize {
        self.0
    }
}

impl ExpandedRange {
    pub const fn new(range: TextRange) -> Self {
        Self(range)
    }

    pub const fn text_range(self) -> TextRange {
        self.0
    }

    pub const fn start(self) -> TextSize {
        self.0.start()
    }

    pub const fn end(self) -> TextSize {
        self.0.end()
    }

    pub const fn is_empty(self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OriginRange(TextRange);

impl OriginRange {
    pub const fn new(range: TextRange) -> Self {
        Self(range)
    }

    pub const fn text_range(self) -> TextRange {
        self.0
    }

    pub const fn start(self) -> TextSize {
        self.0.start()
    }

    pub const fn end(self) -> TextSize {
        self.0.end()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceMapSegment {
    pub output_range: ExpandedRange,
    pub origin: SourceOrigin,
    pub mapping: SourceMapping,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceMapping {
    Identity,
    WholeOrigin,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreprocessedDocument {
    pub source: String,
    source_map: Vec<SourceMapSegment>,
    pub directives: Vec<Directive>,
    pub notices: Vec<PreprocessNotice>,
}

impl PreprocessedDocument {
    fn from_parts(
        source: String,
        source_map: Vec<SourceMapSegment>,
        directives: Vec<Directive>,
        notices: Vec<PreprocessNotice>,
    ) -> Result<Self, SourceMapInvariantError> {
        let source_end = TextSize::new(source.len()).map_err(|_| SourceMapInvariantError)?;
        let mut previous_end = TextSize::ZERO;
        for segment in &source_map {
            if segment.output_range.start() < previous_end
                || segment.output_range.end() > source_end
            {
                return Err(SourceMapInvariantError);
            }
            previous_end = segment.output_range.end();
        }
        Ok(Self {
            source,
            source_map,
            directives,
            notices,
        })
    }

    pub fn source_map(&self) -> &[SourceMapSegment] {
        &self.source_map
    }

    pub fn origin_at(&self, output_offset: ExpandedOffset) -> Option<&SourceOrigin> {
        let output_offset = output_offset.text_size();
        let index = self
            .source_map
            .partition_point(|segment| segment.output_range.end() <= output_offset);
        self.source_map
            .get(index)
            .filter(|segment| segment.output_range.start() <= output_offset)
            .map(|segment| &segment.origin)
    }

    /// Maps an output range to the originating source segment.
    ///
    /// When a range crosses include boundaries, the origin containing its
    /// start is returned. Consumers that need exact pieces should inspect
    /// `source_map` directly.
    pub fn origin_for_range(&self, output_range: ExpandedRange) -> Option<&SourceOrigin> {
        if let Some(origin) = self.origin_at(ExpandedOffset::new(output_range.start())) {
            return Some(origin);
        }
        if !output_range.is_empty() {
            return None;
        }
        self.source_map
            .iter()
            .rev()
            .find(|segment| segment.output_range.end() == output_range.start())
            .map(|segment| &segment.origin)
    }

    /// Projects an expanded range into all originating source ranges.
    ///
    /// Adjacent pieces in the same source are merged. For an unchanged segment,
    /// the relative byte range is preserved. A transformed segment (for example
    /// `indent` or `leveloffset`) conservatively maps to its complete source line.
    pub fn origins_for_range(&self, output_range: ExpandedRange) -> Vec<SourceOrigin> {
        if output_range.is_empty() {
            let segment = self
                .source_map
                .iter()
                .find(|segment| {
                    segment.output_range.start() <= output_range.start()
                        && output_range.start() < segment.output_range.end()
                })
                .or_else(|| {
                    self.source_map
                        .last()
                        .filter(|segment| segment.output_range.end() == output_range.start())
                });
            let Some(segment) = segment else {
                return Vec::new();
            };
            let range = if segment.mapping == SourceMapping::Identity {
                let relative = output_range
                    .start()
                    .to_u32()
                    .saturating_sub(segment.output_range.start().to_u32());
                let offset =
                    TextSize::new(segment.origin.range.start().to_usize() + relative as usize)
                        .expect("projected source offset is bounded");
                TextRange::new(offset, offset).expect("zero source range is ordered")
            } else {
                segment.origin.range.text_range()
            };
            return vec![SourceOrigin {
                source_id: segment.origin.source_id.clone(),
                range: OriginRange::new(range),
            }];
        }
        let mut origins: Vec<SourceOrigin> = Vec::new();
        let first = self
            .source_map
            .partition_point(|segment| segment.output_range.end() <= output_range.start());
        for segment in &self.source_map[first..] {
            if output_range.end() <= segment.output_range.start() {
                break;
            }
            let start = segment
                .output_range
                .start()
                .to_u32()
                .max(output_range.start().to_u32());
            let end = segment
                .output_range
                .end()
                .to_u32()
                .min(output_range.end().to_u32());
            if start >= end {
                continue;
            }

            let range = if segment.mapping == SourceMapping::Identity {
                let relative_start = start.saturating_sub(segment.output_range.start().to_u32());
                let relative_end = end.saturating_sub(segment.output_range.start().to_u32());
                TextRange::new(
                    TextSize::new(
                        segment.origin.range.start().to_usize() + relative_start as usize,
                    )
                    .expect("projected source offset is bounded"),
                    TextSize::new(segment.origin.range.start().to_usize() + relative_end as usize)
                        .expect("projected source offset is bounded"),
                )
                .expect("projected source range is ordered")
            } else {
                segment.origin.range.text_range()
            };
            let origin = SourceOrigin {
                source_id: segment.origin.source_id.clone(),
                range: OriginRange::new(range),
            };
            let merged = if let Some(previous) = origins.last_mut() {
                if previous.source_id == origin.source_id
                    && previous.range.end() == origin.range.start()
                {
                    previous.range = OriginRange::new(
                        TextRange::new(previous.range.start(), origin.range.end())
                            .expect("merged source range is ordered"),
                    );
                    true
                } else {
                    false
                }
            } else {
                false
            };
            if !merged {
                origins.push(origin);
            }
        }
        origins
    }

    fn origins_for_empty_range_within(
        &self,
        output_range: ExpandedRange,
        containing_range: ExpandedRange,
    ) -> Vec<SourceOrigin> {
        debug_assert!(output_range.is_empty());
        let Some(segment) = self.source_map.iter().find(|segment| {
            segment.output_range.start() <= output_range.start()
                && output_range.start() <= segment.output_range.end()
                && segment.output_range.start() < containing_range.end()
                && containing_range.start() < segment.output_range.end()
        }) else {
            return self.origins_for_range(output_range);
        };
        let range = if segment.mapping == SourceMapping::Identity {
            let relative = output_range
                .start()
                .to_u32()
                .saturating_sub(segment.output_range.start().to_u32());
            let offset = TextSize::new(segment.origin.range.start().to_usize() + relative as usize)
                .expect("projected source offset is bounded");
            TextRange::new(offset, offset).expect("zero source range is ordered")
        } else {
            segment.origin.range.text_range()
        };
        vec![SourceOrigin {
            source_id: segment.origin.source_id.clone(),
            range: OriginRange::new(range),
        }]
    }

    fn mapping_is_identity(&self, output_range: ExpandedRange) -> bool {
        if output_range.is_empty() {
            return false;
        }
        let index = self
            .source_map
            .partition_point(|segment| segment.output_range.end() <= output_range.start());
        self.source_map.get(index).is_some_and(|segment| {
            segment.mapping == SourceMapping::Identity
                && segment.output_range.start() <= output_range.start()
                && output_range.end() <= segment.output_range.end()
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SourceMapInvariantError;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Originated<T> {
    pub origins: Vec<SourceOrigin>,
    pub value: T,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectedFix {
    pub title: String,
    pub applicability: crate::diagnostic::Applicability,
    pub applicable: bool,
    pub edits: Vec<Originated<TextEdit>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectedDiagnostic {
    pub diagnostic: Diagnostic,
    pub origins: Vec<SourceOrigin>,
    pub related: Vec<Originated<RelatedInformation>>,
    pub fixes: Vec<ProjectedFix>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectedDocumentSymbol {
    pub symbol: DocumentSymbol,
    pub origins: Vec<SourceOrigin>,
    pub selection_origins: Vec<SourceOrigin>,
    pub children: Vec<ProjectedDocumentSymbol>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectedReference {
    pub value: Reference,
    pub origins: Vec<SourceOrigin>,
    pub target_origins: Vec<SourceOrigin>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectedLocalTarget {
    pub value: crate::local_target::LocalTargetReference,
    pub origins: Vec<SourceOrigin>,
    pub target_origins: Vec<SourceOrigin>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectedResource {
    pub value: ResourceReference,
    pub origins: Vec<SourceOrigin>,
    pub target_origins: Vec<SourceOrigin>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectedDocumentAttribute {
    pub value: crate::attributes::DocumentAttributeOccurrence,
    pub origins: Vec<SourceOrigin>,
    pub name_origins: Vec<SourceOrigin>,
    pub value_origins: Vec<SourceOrigin>,
    pub value_lines: Vec<ProjectedDocumentAttributeValueLine>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectedDocumentAttributeValueLine {
    pub value: crate::attributes::DocumentAttributeValueLine,
    pub origins: Vec<SourceOrigin>,
    pub indent_origins: Vec<SourceOrigin>,
    pub content_origins: Vec<SourceOrigin>,
    pub ending_origins: Vec<SourceOrigin>,
    pub continuation_origins: Vec<SourceOrigin>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectedAttributeBinding {
    pub value: crate::attributes::AttributeBinding,
    pub origins: Vec<SourceOrigin>,
    pub name_origins: Vec<SourceOrigin>,
    pub value_origins: Vec<SourceOrigin>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectedAttributeReference {
    pub value: crate::attributes::AttributeReference,
    pub origins: Vec<SourceOrigin>,
    pub name_origins: Vec<SourceOrigin>,
}

/// All editor-facing facts from an expanded analysis, projected to original sources.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnalysisProjection {
    pub attribute_bindings: Vec<ProjectedAttributeBinding>,
    pub attribute_occurrences: Vec<ProjectedDocumentAttribute>,
    pub attribute_references: Vec<ProjectedAttributeReference>,
    pub directives: Vec<Directive>,
    pub diagnostics: Vec<ProjectedDiagnostic>,
    pub local_targets: Vec<ProjectedLocalTarget>,
    pub references: Vec<ProjectedReference>,
    pub resources: Vec<ProjectedResource>,
    pub symbols: Vec<ProjectedDocumentSymbol>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProjectionLimits {
    pub max_origin_segments: u32,
}

impl Default for ProjectionLimits {
    fn default() -> Self {
        Self {
            max_origin_segments: 1_000_000,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProjectionError {
    pub limit: u32,
    pub actual: u64,
}

impl fmt::Display for ProjectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "projection origin segment limit exceeded (limit {}, actual {})",
            self.limit, self.actual
        )
    }
}

impl Error for ProjectionError {}

/// Analysis paired with the source map used to build it.
#[derive(Debug)]
pub struct PreprocessedAnalysis {
    pub document: PreprocessedDocument,
    pub analysis: Analysis,
}

impl PreprocessedAnalysis {
    pub fn project_origins(
        &self,
        limits: ProjectionLimits,
    ) -> Result<AnalysisProjection, ProjectionError> {
        let map = &self.document;
        let mut projected_segments = 0_u64;
        let attribute_occurrences = self
            .analysis
            .document_attribute_occurrences()
            .iter()
            .cloned()
            .map(|value| {
                let origins = project_attribute_range(
                    map,
                    value.range,
                    value.range,
                    &mut projected_segments,
                    limits,
                )?;
                let name_origins = project_attribute_range(
                    map,
                    value.name_range,
                    value.range,
                    &mut projected_segments,
                    limits,
                )?;
                let value_origins = project_attribute_range(
                    map,
                    value.value.source_range,
                    value.range,
                    &mut projected_segments,
                    limits,
                )?;
                let value_lines = value
                    .value
                    .lines
                    .iter()
                    .cloned()
                    .map(|line| {
                        let origins = project_attribute_range(
                            map,
                            line.range,
                            value.range,
                            &mut projected_segments,
                            limits,
                        )?;
                        let indent_origins = project_attribute_range(
                            map,
                            line.indent_range,
                            value.range,
                            &mut projected_segments,
                            limits,
                        )?;
                        let content_origins = project_attribute_range(
                            map,
                            line.content_range,
                            value.range,
                            &mut projected_segments,
                            limits,
                        )?;
                        let ending_origins = project_attribute_range(
                            map,
                            line.ending_range,
                            value.range,
                            &mut projected_segments,
                            limits,
                        )?;
                        let continuation_origins = line
                            .continuation
                            .map(|continuation| {
                                project_attribute_range(
                                    map,
                                    continuation.range,
                                    value.range,
                                    &mut projected_segments,
                                    limits,
                                )
                            })
                            .transpose()?
                            .unwrap_or_default();
                        Ok(ProjectedDocumentAttributeValueLine {
                            value: line,
                            origins,
                            indent_origins,
                            content_origins,
                            ending_origins,
                            continuation_origins,
                        })
                    })
                    .collect::<Result<Vec<_>, ProjectionError>>()?;
                Ok(ProjectedDocumentAttribute {
                    value,
                    origins,
                    name_origins,
                    value_origins,
                    value_lines,
                })
            })
            .collect::<Result<Vec<_>, ProjectionError>>()?;
        let attribute_bindings = self
            .analysis
            .attribute_environment()
            .bindings()
            .iter()
            .cloned()
            .map(|value| {
                let occurrence = value.occurrence();
                Ok(ProjectedAttributeBinding {
                    origins: project_attribute_range(
                        map,
                        occurrence.range,
                        occurrence.range,
                        &mut projected_segments,
                        limits,
                    )?,
                    name_origins: project_attribute_range(
                        map,
                        occurrence.name_range,
                        occurrence.range,
                        &mut projected_segments,
                        limits,
                    )?,
                    value_origins: project_attribute_range(
                        map,
                        occurrence.value.source_range,
                        occurrence.range,
                        &mut projected_segments,
                        limits,
                    )?,
                    value,
                })
            })
            .collect::<Result<Vec<_>, ProjectionError>>()?;
        let attribute_references = self
            .analysis
            .attribute_references()
            .iter()
            .cloned()
            .map(|value| {
                let origins = project_attribute_range(
                    map,
                    value.range,
                    value.range,
                    &mut projected_segments,
                    limits,
                )?;
                let name_origins = project_attribute_range(
                    map,
                    value.name_range,
                    value.range,
                    &mut projected_segments,
                    limits,
                )?;
                Ok(ProjectedAttributeReference {
                    value,
                    origins,
                    name_origins,
                })
            })
            .collect::<Result<Vec<_>, ProjectionError>>()?;
        let mut project = |range| {
            let origins = map.origins_for_range(ExpandedRange::new(range));
            projected_segments = projected_segments.saturating_add(origins.len() as u64);
            if projected_segments > u64::from(limits.max_origin_segments) {
                Err(ProjectionError {
                    limit: limits.max_origin_segments,
                    actual: projected_segments,
                })
            } else {
                Ok(origins)
            }
        };
        let diagnostics = self
            .analysis
            .diagnostics()
            .iter()
            .cloned()
            .map(|diagnostic| {
                let origins = project(diagnostic.range)?;
                let related = diagnostic
                    .related
                    .iter()
                    .cloned()
                    .map(|value| {
                        Ok(Originated {
                            origins: project(value.range)?,
                            value,
                        })
                    })
                    .collect::<Result<Vec<_>, ProjectionError>>()?;
                let fixes = diagnostic
                    .fixes
                    .iter()
                    .cloned()
                    .map(|fix| -> Result<_, ProjectionError> {
                        let edits: Vec<_> = fix
                            .edits()
                            .iter()
                            .cloned()
                            .map(|value| {
                                Ok(Originated {
                                    origins: project(value.range)?,
                                    value,
                                })
                            })
                            .collect::<Result<_, ProjectionError>>()?;
                        let applicable = edits.iter().all(|edit| edit.origins.len() == 1)
                            && edits.iter().all(|edit| {
                                map.mapping_is_identity(ExpandedRange::new(edit.value.range))
                            });
                        Ok(ProjectedFix {
                            title: fix.title,
                            applicability: fix.applicability,
                            applicable,
                            edits,
                        })
                    })
                    .collect::<Result<Vec<_>, ProjectionError>>()?;
                Ok(ProjectedDiagnostic {
                    diagnostic,
                    origins,
                    related,
                    fixes,
                })
            })
            .collect::<Result<Vec<_>, ProjectionError>>()?;
        let mut local_targets = Vec::new();
        for link in self.analysis.links() {
            let Some(value) = crate::local_target::LocalTargetReference::from_link(link) else {
                continue;
            };
            local_targets.push(ProjectedLocalTarget {
                origins: project(value.range)?,
                target_origins: project(value.target_range)?,
                value,
            });
        }
        let mut references = Vec::new();
        for value in self.analysis.references() {
            let origins = project(value.range)?;
            let target_origins = project(value.target_range)?;
            if let Some(local) = crate::local_target::LocalTargetReference::from_reference(value) {
                let local_target_origins = project(local.target_range)?;
                local_targets.push(ProjectedLocalTarget {
                    value: local,
                    origins: origins.clone(),
                    target_origins: local_target_origins,
                });
            }
            references.push(ProjectedReference {
                origins,
                target_origins,
                value: value.clone(),
            });
        }
        let mut resources = Vec::new();
        for value in self.analysis.resources() {
            let origins = project(value.range())?;
            let target_origins = project(value.target_range())?;
            if let Some(local) = crate::local_target::LocalTargetReference::from_resource(value) {
                local_targets.push(ProjectedLocalTarget {
                    value: local,
                    origins: origins.clone(),
                    target_origins: target_origins.clone(),
                });
            }
            resources.push(ProjectedResource {
                origins,
                target_origins,
                value: value.clone(),
            });
        }
        for directive in &self.document.directives {
            let Some(value) = directive.local_target() else {
                continue;
            };
            let origin = SourceOrigin {
                source_id: directive.source_id.clone(),
                range: OriginRange::new(directive.range),
            };
            let target_origin = SourceOrigin {
                source_id: directive.source_id.clone(),
                range: OriginRange::new(directive.target_range),
            };
            local_targets.push(ProjectedLocalTarget {
                value,
                origins: vec![origin],
                target_origins: vec![target_origin],
            });
        }
        let symbols = crate::document::document_symbols(self.analysis.document())
            .into_iter()
            .map(|symbol| project_symbol(symbol, &mut project))
            .collect::<Result<Vec<_>, ProjectionError>>()?;
        Ok(AnalysisProjection {
            attribute_bindings,
            attribute_occurrences,
            attribute_references,
            directives: self.document.directives.clone(),
            diagnostics,
            local_targets,
            references,
            resources,
            symbols,
        })
    }
}

fn project_attribute_range(
    map: &PreprocessedDocument,
    range: TextRange,
    occurrence_range: TextRange,
    projected_segments: &mut u64,
    limits: ProjectionLimits,
) -> Result<Vec<SourceOrigin>, ProjectionError> {
    let origins = if range.is_empty() {
        map.origins_for_empty_range_within(
            ExpandedRange::new(range),
            ExpandedRange::new(occurrence_range),
        )
    } else {
        map.origins_for_range(ExpandedRange::new(range))
    };
    *projected_segments = projected_segments.saturating_add(origins.len() as u64);
    if *projected_segments > u64::from(limits.max_origin_segments) {
        Err(ProjectionError {
            limit: limits.max_origin_segments,
            actual: *projected_segments,
        })
    } else {
        Ok(origins)
    }
}

fn project_symbol(
    mut symbol: DocumentSymbol,
    project: &mut impl FnMut(TextRange) -> Result<Vec<SourceOrigin>, ProjectionError>,
) -> Result<ProjectedDocumentSymbol, ProjectionError> {
    let children = std::mem::take(&mut symbol.children)
        .into_iter()
        .map(|child| project_symbol(child, project))
        .collect::<Result<_, _>>()?;
    Ok(ProjectedDocumentSymbol {
        origins: project(symbol.range)?,
        selection_origins: project(symbol.selection_range)?,
        symbol,
        children,
    })
}

#[derive(Debug)]
pub enum PreprocessedAnalysisError {
    Preprocess(PreprocessError),
    Parse(ParseError),
}

impl fmt::Display for PreprocessedAnalysisError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Preprocess(error) => error.fmt(formatter),
            Self::Parse(error) => error.fmt(formatter),
        }
    }
}

impl Error for PreprocessedAnalysisError {}

/// Expands a caller-provided snapshot and analyzes the resulting text.
pub fn preprocess_and_analyze(
    engine: &Engine,
    source: &str,
    snapshot: &ResourceSnapshot,
    options: &PreprocessOptions,
) -> Result<PreprocessedAnalysis, PreprocessedAnalysisError> {
    let document =
        preprocess(source, snapshot, options).map_err(PreprocessedAnalysisError::Preprocess)?;
    let analysis = engine
        .analyze(&document.source)
        .map_err(PreprocessedAnalysisError::Parse)?;
    Ok(PreprocessedAnalysis { document, analysis })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreprocessErrorKind {
    MissingResource,
    IncludeCycle,
    DepthLimit,
    IncludeLimit,
    ByteLimit,
    NodeLimit,
    SourceMapLimit,
    UnsafeTarget,
    InvalidDirective,
    UnsupportedEncoding,
    UnclosedConditional,
    InternalInvariant,
}

impl PreprocessErrorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MissingResource => "missing-resource",
            Self::IncludeCycle => "include-cycle",
            Self::DepthLimit => "depth-limit",
            Self::IncludeLimit => "include-limit",
            Self::ByteLimit => "byte-limit",
            Self::NodeLimit => "node-limit",
            Self::SourceMapLimit => "source-map-limit",
            Self::UnsafeTarget => "unsafe-target",
            Self::InvalidDirective => "invalid-directive",
            Self::UnsupportedEncoding => "unsupported-encoding",
            Self::UnclosedConditional => "unclosed-conditional",
            Self::InternalInvariant => "internal-invariant",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreprocessError {
    pub kind: PreprocessErrorKind,
    pub source_id: Option<SourceId>,
    pub range: TextRange,
    /// Expanded target before resolution against the current include base.
    pub requested_target: Option<String>,
    /// Snapshot key after resolution against the current include base.
    pub target: Option<String>,
    pub message: String,
}

impl fmt::Display for PreprocessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for PreprocessError {}

pub fn preprocess(
    source: &str,
    snapshot: &ResourceSnapshot,
    options: &PreprocessOptions,
) -> Result<PreprocessedDocument, PreprocessError> {
    let analysis_limits = crate::limits::AnalysisLimits::default();
    let mut context = Context {
        snapshot,
        options,
        output: String::new(),
        source_map: Vec::new(),
        directives: Vec::new(),
        notices: Vec::new(),
        active: Vec::new(),
        expanded_nodes: 0,
        includes: 0,
        attributes: crate::attributes::SequentialAttributeState::with_locked_values(
            &options.attributes,
            AttributeExpansionLimits {
                max_depth: analysis_limits.max_attribute_expansion_depth,
                max_bytes: analysis_limits.max_attribute_expansion_bytes,
            },
        ),
        attribute_delimiters: Vec::new(),
        attribute_position: true,
    };
    context.expand(
        source,
        options.source_id.clone(),
        0,
        options.base_uri.as_deref(),
    )?;
    PreprocessedDocument::from_parts(
        context.output,
        context.source_map,
        context.directives,
        context.notices,
    )
    .map_err(|_| PreprocessError {
        kind: PreprocessErrorKind::InternalInvariant,
        source_id: options.source_id.clone(),
        range: TextRange::new(TextSize::ZERO, TextSize::ZERO).expect("zero range is ordered"),
        requested_target: None,
        target: None,
        message: "source map segments are unsorted, overlapping, or outside expanded source"
            .to_owned(),
    })
}

struct Context<'a> {
    snapshot: &'a ResourceSnapshot,
    options: &'a PreprocessOptions,
    output: String,
    source_map: Vec<SourceMapSegment>,
    directives: Vec<Directive>,
    notices: Vec<PreprocessNotice>,
    active: Vec<String>,
    expanded_nodes: u64,
    includes: u64,
    attributes: crate::attributes::SequentialAttributeState,
    attribute_delimiters: Vec<String>,
    attribute_position: bool,
}

impl Context<'_> {
    fn expand(
        &mut self,
        source: &str,
        source_id: Option<SourceId>,
        depth: u32,
        base_uri: Option<&str>,
    ) -> Result<(), PreprocessError> {
        let mut offset = 0;
        let lines = source
            .split_inclusive('\n')
            .map(|line| {
                let start = offset;
                offset += line.len();
                SelectedLine {
                    text: line.to_owned(),
                    range: range(start, offset),
                    mapping: SourceMapping::Identity,
                }
            })
            .collect();
        self.expand_selected(lines, source_id, depth, base_uri)
    }

    fn expand_include(
        &mut self,
        include: ParsedDirective,
        source_id: Option<SourceId>,
        range: TextRange,
        depth: u32,
        base_uri: Option<&str>,
    ) -> Result<(), PreprocessError> {
        if depth >= self.options.max_include_depth {
            return Err(error(
                PreprocessErrorKind::DepthLimit,
                source_id.clone(),
                range,
                "include depth limit exceeded",
            ));
        }
        self.includes += 1;
        if self.includes > u64::from(self.options.max_includes) {
            return Err(error(
                PreprocessErrorKind::IncludeLimit,
                source_id,
                range,
                "include count limit exceeded",
            ));
        }
        self.bump_node(source_id.clone(), range)?;
        let expanded_target = expand_attributes(&include.target, self.attributes.values());
        let target = resolve_include_target(&expanded_target, base_uri);
        validate_target(&target, self.options).map_err(|message| {
            error(
                PreprocessErrorKind::UnsafeTarget,
                source_id.clone(),
                range,
                message,
            )
        })?;
        if self.active.contains(&target) {
            return Err(error(
                PreprocessErrorKind::IncludeCycle,
                source_id,
                range,
                "include cycle detected",
            ));
        }
        let attributes = parse_attributes(&include.attributes).map_err(|message| {
            error(
                PreprocessErrorKind::InvalidDirective,
                source_id.clone(),
                range,
                message,
            )
        })?;
        let optional = attributes.contains_key("optional");
        if let Some(encoding) = attributes.get("encoding")
            && !encoding.eq_ignore_ascii_case("utf-8")
            && !encoding.eq_ignore_ascii_case("utf8")
        {
            return Err(error(
                PreprocessErrorKind::UnsupportedEncoding,
                source_id,
                range,
                "resource snapshots contain UTF-8 text only",
            ));
        }
        let document = self.snapshot.get(&target);
        self.directives.push(Directive {
            kind: DirectiveKind::Include,
            source_id: source_id.clone(),
            range,
            authored_target: Some(expanded_target.clone()),
            optional,
            target: target.clone(),
            target_range: relative_range(range, include.target_start, include.target_end),
            resource_source_id: document.map(|document| document.source_id.clone()),
        });
        let Some(document) = document else {
            if optional {
                self.notices.push(PreprocessNotice {
                    kind: PreprocessNoticeKind::OptionalResourceMissing,
                    source_id,
                    range,
                    target,
                });
                return Ok(());
            }
            return Err(PreprocessError {
                kind: PreprocessErrorKind::MissingResource,
                source_id,
                range,
                requested_target: Some(expanded_target),
                target: Some(target.clone()),
                message: format!("resource snapshot does not contain {target}"),
            });
        };
        let selected = select_lines(&document.source, &attributes);
        let transformed = transform_lines(selected, &attributes);
        let nested_base = target_base(&target);
        self.active.push(target);
        self.expand_selected(
            transformed,
            Some(document.source_id.clone()),
            depth + 1,
            nested_base.as_deref(),
        )?;
        self.active.pop();
        Ok(())
    }

    fn expand_selected(
        &mut self,
        lines: Vec<SelectedLine>,
        source_id: Option<SourceId>,
        depth: u32,
        base_uri: Option<&str>,
    ) -> Result<(), PreprocessError> {
        let selected_source = lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<String>();
        let selected_document = crate::source_document::SourceDocument::new(&selected_source)
            .map_err(|_| {
                error(
                    PreprocessErrorKind::InternalInvariant,
                    source_id.clone(),
                    zero_range(),
                    "selected source exceeds the supported position range",
                )
            })?;
        if selected_document.lines().len() < lines.len() {
            return Err(error(
                PreprocessErrorKind::InternalInvariant,
                source_id,
                zero_range(),
                "selected source lines do not preserve physical boundaries",
            ));
        }
        let mut conditions = Vec::<bool>::new();
        let mut attribute_value_through = None;
        for (line_index, line) in lines.into_iter().enumerate() {
            let content = line.text.trim_end_matches(['\r', '\n']);
            let enabled = conditions.iter().all(|condition| *condition);
            if attribute_value_through.is_some_and(|last_line| line_index <= last_line) {
                self.bump_node(source_id.clone(), line.range)?;
                self.append(&line.text, source_id.clone(), line.range, line.mapping)?;
                if attribute_value_through == Some(line_index) {
                    attribute_value_through = None;
                }
                continue;
            }
            if let Some(directive) = conditional_directive(content) {
                self.bump_node(source_id.clone(), line.range)?;
                self.directives.push(Directive {
                    kind: directive.kind,
                    source_id: source_id.clone(),
                    range: line.range,
                    authored_target: None,
                    optional: false,
                    target: directive.target.clone(),
                    target_range: relative_range(
                        line.range,
                        directive.target_start,
                        directive.target_end,
                    ),
                    resource_source_id: None,
                });
                match directive.kind {
                    DirectiveKind::Ifdef | DirectiveKind::Ifndef
                        if !directive.attributes.is_empty() =>
                    {
                        let present = directive.kind == DirectiveKind::Ifdef;
                        if enabled
                            && conditional_attribute(
                                &directive.target,
                                self.attributes.values(),
                                present,
                            )
                        {
                            let ending = &line.text[content.len()..];
                            self.append(
                                &format!("{}{ending}", directive.attributes),
                                source_id.clone(),
                                line.range,
                                SourceMapping::WholeOrigin,
                            )?;
                            self.attribute_position = false;
                        }
                    }
                    DirectiveKind::Ifdef => conditions.push(
                        enabled
                            && conditional_attribute(
                                &directive.target,
                                self.attributes.values(),
                                true,
                            ),
                    ),
                    DirectiveKind::Ifndef => conditions.push(
                        enabled
                            && conditional_attribute(
                                &directive.target,
                                self.attributes.values(),
                                false,
                            ),
                    ),
                    DirectiveKind::Ifeval => conditions.push(
                        enabled
                            && evaluate_expression(&expand_attributes(
                                &directive.attributes,
                                self.attributes.values(),
                            )),
                    ),
                    DirectiveKind::Endif => {
                        if conditions.pop().is_none() {
                            return Err(error(
                                PreprocessErrorKind::InvalidDirective,
                                source_id,
                                line.range,
                                "endif has no matching conditional",
                            ));
                        }
                    }
                    DirectiveKind::Include => unreachable!(),
                }
            } else if enabled {
                let delimiter = crate::delimiter::spec(content).is_some();
                if delimiter {
                    if self
                        .attribute_delimiters
                        .last()
                        .is_some_and(|open| open == content)
                    {
                        self.attribute_delimiters.pop();
                    } else {
                        self.attribute_delimiters.push(content.to_owned());
                    }
                }
                if let Some(include) = include_directive(content) {
                    if self.options.enable_includes {
                        self.expand_include(
                            include,
                            source_id.clone(),
                            line.range,
                            depth,
                            base_uri,
                        )?;
                    } else {
                        self.bump_node(source_id.clone(), line.range)?;
                        let authored_target =
                            expand_attributes(&include.target, self.attributes.values());
                        let optional = parse_attributes(&include.attributes)
                            .is_ok_and(|attributes| attributes.contains_key("optional"));
                        self.directives.push(Directive {
                            kind: DirectiveKind::Include,
                            source_id: source_id.clone(),
                            range: line.range,
                            authored_target: Some(authored_target),
                            optional,
                            target: include.target,
                            target_range: relative_range(
                                line.range,
                                include.target_start,
                                include.target_end,
                            ),
                            resource_source_id: None,
                        });
                        self.append(&line.text, source_id.clone(), line.range, line.mapping)?;
                        self.attribute_position = false;
                    }
                } else if let Some(literal) = escaped_directive(content) {
                    let ending = &line.text[content.len()..];
                    self.append(
                        &format!("{literal}{ending}"),
                        source_id.clone(),
                        line.range,
                        SourceMapping::WholeOrigin,
                    )?;
                    self.attribute_position = false;
                } else {
                    let mut document_attribute = false;
                    self.bump_node(source_id.clone(), line.range)?;
                    if !delimiter
                        && self.attribute_delimiters.is_empty()
                        && self.attribute_position
                        && crate::attributes::parse_line(
                            content,
                            selected_document.lines()[line_index]
                                .content_range()
                                .start()
                                .to_usize(),
                            selected_document.lines()[line_index].full_range(),
                        )
                        .is_some()
                        && let Some((occurrence, _, last_line)) =
                            crate::attributes::parse_lines(&selected_document, line_index, &|| {
                                false
                            })
                            .map_err(|_| {
                                error(
                                    PreprocessErrorKind::InternalInvariant,
                                    source_id.clone(),
                                    line.range,
                                    "attribute preprocessing failed",
                                )
                            })?
                    {
                        let _ = self.attributes.apply(&occurrence);
                        document_attribute = true;
                        if last_line > line_index {
                            attribute_value_through = Some(last_line);
                        }
                    }
                    self.append(&line.text, source_id.clone(), line.range, line.mapping)?;
                    self.attribute_position = document_attribute
                        || content.trim_matches([' ', '\t']).is_empty()
                        || content.starts_with("//");
                }
            }
        }
        if !conditions.is_empty() {
            return Err(error(
                PreprocessErrorKind::UnclosedConditional,
                source_id,
                zero_range(),
                "conditional directive is not closed",
            ));
        }
        Ok(())
    }

    fn bump_node(
        &mut self,
        source_id: Option<SourceId>,
        range: TextRange,
    ) -> Result<(), PreprocessError> {
        self.expanded_nodes += 1;
        if self.expanded_nodes > u64::from(self.options.max_expanded_nodes) {
            return Err(error(
                PreprocessErrorKind::NodeLimit,
                source_id,
                range,
                "preprocessor node limit exceeded",
            ));
        }
        Ok(())
    }

    fn append(
        &mut self,
        value: &str,
        source_id: Option<SourceId>,
        origin_range: TextRange,
        mapping: SourceMapping,
    ) -> Result<(), PreprocessError> {
        let start = self.output.len();
        let end = start.saturating_add(value.len());
        if end > self.options.max_total_bytes as usize {
            return Err(error(
                PreprocessErrorKind::ByteLimit,
                source_id,
                origin_range,
                "preprocessor byte limit exceeded",
            ));
        }
        self.output.push_str(value);
        if start < end {
            if self.source_map.len() >= self.options.max_source_map_segments as usize {
                return Err(error(
                    PreprocessErrorKind::SourceMapLimit,
                    source_id,
                    origin_range,
                    "source map segment limit exceeded",
                ));
            }
            self.source_map.push(SourceMapSegment {
                output_range: ExpandedRange::new(range(start, end)),
                origin: SourceOrigin {
                    source_id,
                    range: OriginRange::new(origin_range),
                },
                mapping,
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct ParsedDirective {
    kind: DirectiveKind,
    target: String,
    attributes: String,
    target_start: usize,
    target_end: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IncludeRequest {
    pub range: TextRange,
    pub target_range: TextRange,
    pub target: String,
    pub attributes: String,
}

/// Finds syntactically complete, unescaped include directives without performing I/O.
///
/// Hosts may load a superset of resources from these requests. Conditional evaluation and
/// authoritative target validation remain the responsibility of [`preprocess`].
pub fn discover_includes(source: &str) -> Result<Vec<IncludeRequest>, PositionError> {
    TextSize::new(source.len())?;
    let mut requests = Vec::new();
    let mut offset = 0;
    for line in source.split_inclusive('\n') {
        let end = offset + line.len();
        let content = line.trim_end_matches(['\r', '\n']);
        if !content.starts_with('\\')
            && let Some(include) = include_directive(content)
        {
            requests.push(IncludeRequest {
                range: TextRange::new(TextSize::new(offset)?, TextSize::new(end)?)?,
                target_range: TextRange::new(
                    TextSize::new(offset + include.target_start)?,
                    TextSize::new(offset + include.target_end)?,
                )?,
                target: include.target,
                attributes: include.attributes,
            });
        }
        offset = end;
    }
    Ok(requests)
}

fn include_directive(value: &str) -> Option<ParsedDirective> {
    parse_directive(value, "include::", DirectiveKind::Include)
}

fn conditional_directive(value: &str) -> Option<ParsedDirective> {
    [
        ("ifdef::", DirectiveKind::Ifdef),
        ("ifndef::", DirectiveKind::Ifndef),
        ("ifeval::", DirectiveKind::Ifeval),
        ("endif::", DirectiveKind::Endif),
    ]
    .into_iter()
    .find_map(|(prefix, kind)| parse_directive(value, prefix, kind))
}

fn parse_directive(value: &str, prefix: &str, kind: DirectiveKind) -> Option<ParsedDirective> {
    let rest = value.strip_prefix(prefix)?;
    let bracket = rest.find('[')?;
    let close = rest.rfind(']')?;
    (close == rest.len() - 1 && bracket <= close).then(|| ParsedDirective {
        kind,
        target: rest[..bracket].to_owned(),
        attributes: rest[bracket + 1..close].to_owned(),
        target_start: prefix.len(),
        target_end: prefix.len() + bracket,
    })
}

fn escaped_directive(value: &str) -> Option<&str> {
    let literal = value.strip_prefix('\\')?;
    (include_directive(literal).is_some() || conditional_directive(literal).is_some())
        .then_some(literal)
}

fn conditional_attribute(
    target: &str,
    attributes: &BTreeMap<String, String>,
    present: bool,
) -> bool {
    let matches = if target.contains('+') {
        target
            .split('+')
            .all(|name| attributes.contains_key(name.trim()))
    } else {
        target
            .split(',')
            .any(|name| attributes.contains_key(name.trim()))
    };
    if present { matches } else { !matches }
}

fn evaluate_expression(value: &str) -> bool {
    for operator in ["==", "!=", ">=", "<=", ">", "<"] {
        if let Some((left, right)) = value.split_once(operator) {
            let left = left.trim().trim_matches(['\'', '"']);
            let right = right.trim().trim_matches(['\'', '"']);
            let numeric = left.parse::<f64>().ok().zip(right.parse::<f64>().ok());
            return match (operator, numeric) {
                ("==", _) => left == right,
                ("!=", _) => left != right,
                (">=", Some((left, right))) => left >= right,
                ("<=", Some((left, right))) => left <= right,
                (">", Some((left, right))) => left > right,
                ("<", Some((left, right))) => left < right,
                _ => false,
            };
        }
    }
    false
}

fn expand_attributes(value: &str, attributes: &BTreeMap<String, String>) -> String {
    let mut output = String::new();
    let mut cursor = 0;
    while let Some(open) = value[cursor..].find('{').map(|offset| cursor + offset) {
        output.push_str(&value[cursor..open]);
        let Some(close) = value[open + 1..].find('}').map(|offset| open + 1 + offset) else {
            output.push_str(&value[open..]);
            return output;
        };
        let name = &value[open + 1..close];
        if let Some(replacement) = attributes.get(name) {
            output.push_str(replacement);
        } else {
            output.push_str(&value[open..=close]);
        }
        cursor = close + 1;
    }
    output.push_str(&value[cursor..]);
    output
}

fn parse_attributes(value: &str) -> Result<BTreeMap<String, String>, String> {
    let mut fields = Vec::new();
    let mut start = 0;
    let mut quote = None;
    for (index, character) in value.char_indices() {
        match quote {
            Some(active) if character == active => quote = None,
            Some(_) => {}
            None if matches!(character, '\'' | '"') => quote = Some(character),
            None if character == ',' => {
                fields.push(&value[start..index]);
                start = index + 1;
            }
            None => {}
        }
    }
    if quote.is_some() {
        return Err("include attribute list has an unclosed quote".to_owned());
    }
    fields.push(&value[start..]);
    let mut attributes = BTreeMap::new();
    for field in fields {
        let field = field.trim();
        if field.is_empty() {
            continue;
        }
        if let Some((name, value)) = field.split_once('=') {
            let name = name.trim();
            let value = value.trim();
            if name.is_empty() {
                return Err("include attribute name is empty".to_owned());
            }
            let quoted = value
                .strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
                .or_else(|| {
                    value
                        .strip_prefix('"')
                        .and_then(|value| value.strip_suffix('"'))
                });
            if (value.starts_with(['\'', '"']) || value.ends_with(['\'', '"'])) && quoted.is_none()
            {
                return Err("include attribute quote is malformed".to_owned());
            }
            attributes.insert(name.to_owned(), quoted.unwrap_or(value).to_owned());
        } else {
            attributes.insert(field.to_owned(), String::new());
        }
    }
    Ok(attributes)
}

#[derive(Clone)]
struct SelectedLine {
    text: String,
    range: TextRange,
    mapping: SourceMapping,
}

fn select_lines(source: &str, attributes: &BTreeMap<String, String>) -> Vec<SelectedLine> {
    let requested_tags = attributes
        .get("tag")
        .into_iter()
        .chain(attributes.get("tags"))
        .flat_map(|value| value.split([';', ',']))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect::<BTreeSet<_>>();
    let requested_lines = attributes
        .get("lines")
        .map(|value| parse_line_selection(value));
    let mut active_tags = Vec::<String>::new();
    let mut offset = 0;
    let mut output = Vec::new();
    for (index, line) in source.split_inclusive('\n').enumerate() {
        let content = line.trim_end_matches(['\r', '\n']);
        if let Some(tag) = tag_marker(content, "tag::") {
            active_tags.push(tag.to_owned());
            offset += line.len();
            continue;
        }
        if let Some(tag) = tag_marker(content, "end::") {
            if let Some(position) = active_tags.iter().rposition(|active| active == tag) {
                active_tags.remove(position);
            }
            offset += line.len();
            continue;
        }
        let number = index + 1;
        let tag_selected = requested_tags.is_empty()
            || active_tags
                .iter()
                .any(|tag| requested_tags.contains(tag.as_str()));
        let line_selected = requested_lines
            .as_ref()
            .is_none_or(|lines| lines.contains(&number));
        if tag_selected && line_selected {
            output.push(SelectedLine {
                text: line.to_owned(),
                range: range(offset, offset + line.len()),
                mapping: SourceMapping::Identity,
            });
        }
        offset += line.len();
    }
    output
}

fn tag_marker<'a>(value: &'a str, marker: &str) -> Option<&'a str> {
    let offset = value.find(marker)?;
    let rest = &value[offset + marker.len()..];
    rest.strip_suffix("[]")
}

fn parse_line_selection(value: &str) -> BTreeSet<usize> {
    let mut output = BTreeSet::new();
    for item in value.split([';', ',']) {
        if let Some((start, end)) = item.trim().split_once("..") {
            if let (Ok(start), Ok(end)) = (start.parse::<usize>(), end.parse::<usize>()) {
                output.extend(start..=end);
            }
        } else if let Ok(line) = item.trim().parse() {
            output.insert(line);
        }
    }
    output
}

fn transform_lines(
    lines: Vec<SelectedLine>,
    attributes: &BTreeMap<String, String>,
) -> Vec<SelectedLine> {
    let indent = attributes
        .get("indent")
        .and_then(|value| value.parse::<i32>().ok())
        .unwrap_or(0);
    let leveloffset = attributes
        .get("leveloffset")
        .and_then(|value| value.parse::<i32>().ok())
        .unwrap_or(0);
    lines
        .into_iter()
        .map(|mut line| {
            let original = line.text.clone();
            if leveloffset != 0 {
                line.text = apply_leveloffset(&line.text, leveloffset);
            }
            if indent > 0 {
                line.text = format!("{}{}", " ".repeat(indent as usize), line.text);
            } else if indent < 0 {
                let remove = (-indent) as usize;
                let leading = line
                    .text
                    .bytes()
                    .take_while(|byte| *byte == b' ')
                    .count()
                    .min(remove);
                line.text.drain(..leading);
            }
            if line.text != original {
                line.mapping = SourceMapping::WholeOrigin;
            }
            line
        })
        .collect()
}

fn apply_leveloffset(line: &str, offset: i32) -> String {
    let marker_count = line.bytes().take_while(|byte| *byte == b'=').count();
    if marker_count == 0 || line.as_bytes().get(marker_count) != Some(&b' ') {
        return line.to_owned();
    }
    let adjusted = (marker_count as i32 + offset).clamp(1, 6) as usize;
    format!("{}{}", "=".repeat(adjusted), &line[marker_count..])
}

fn validate_target(target: &str, options: &PreprocessOptions) -> Result<(), &'static str> {
    if target.is_empty()
        || target.chars().any(|character| character.is_control())
        || target.starts_with('/')
        || target.starts_with('\\')
        || target.contains('\\')
        || target.split('/').any(|segment| segment == "..")
    {
        return Err("unsafe include target");
    }
    if let Some((scheme, _)) = target.split_once(':')
        && (options.safe_mode == SafeMode::Secure
            || !options
                .allowed_schemes
                .contains(&scheme.to_ascii_lowercase()))
    {
        return Err("include target scheme is not allowed");
    }
    Ok(())
}

pub fn resolve_include_target(target: &str, base_uri: Option<&str>) -> String {
    if target.contains(':') || target.starts_with('/') || target.starts_with('\\') {
        return target.to_owned();
    }
    if let Some(base_uri) = base_uri.filter(|base| base.contains(':')) {
        return format!("{}/{target}", base_uri.trim_end_matches('/'));
    }
    let combined = base_uri
        .filter(|base| !base.is_empty())
        .map_or_else(|| target.to_owned(), |base| format!("{base}/{target}"));
    let mut segments = Vec::new();
    for segment in combined.split('/') {
        match segment {
            "" | "." => {}
            ".." if segments.last().is_some_and(|segment| *segment != "..") => {
                segments.pop();
            }
            _ => segments.push(segment),
        }
    }
    segments.join("/")
}

fn target_base(target: &str) -> Option<String> {
    target
        .rsplit_once('/')
        .map(|(base, _)| base.to_owned())
        .filter(|base| !base.is_empty())
}

fn error(
    kind: PreprocessErrorKind,
    source_id: Option<SourceId>,
    range: TextRange,
    message: impl Into<String>,
) -> PreprocessError {
    PreprocessError {
        kind,
        source_id,
        range,
        requested_target: None,
        target: None,
        message: message.into(),
    }
}

fn relative_range(line: TextRange, start: usize, end: usize) -> TextRange {
    TextRange::new(
        TextSize::new(line.start().to_usize() + start).expect("directive input is bounded"),
        TextSize::new(line.start().to_usize() + end).expect("directive input is bounded"),
    )
    .expect("directive target range is ordered")
}

fn range(start: usize, end: usize) -> TextRange {
    TextRange::new(
        TextSize::new(start).expect("preprocessor input is bounded"),
        TextSize::new(end).expect("preprocessor input is bounded"),
    )
    .expect("preprocessor range is ordered")
}

fn zero_range() -> TextRange {
    range(0, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn include_conditionals_filters_and_source_map_are_deterministic() {
        let mut snapshot = ResourceSnapshot::default();
        snapshot.insert(
            "part.adoc",
            ResourceDocument {
                source_id: SourceId::new("part"),
                source: "// tag::keep[]\n= Included\nline one\nline two\n// end::keep[]\n"
                    .to_owned(),
            },
        );
        let mut options = PreprocessOptions {
            source_id: Some(SourceId::new("root")),
            ..PreprocessOptions::default()
        };
        options
            .attributes
            .insert("enabled".to_owned(), "".to_owned());
        let source = "ifdef::enabled[]\ninclude::part.adoc[tag=keep,lines=2..3,leveloffset=+1,indent=2]\nendif::[]\n";
        let result = preprocess(source, &snapshot, &options).expect("preprocess");
        assert_eq!(result.source, "  == Included\n  line one\n");
        assert_eq!(result.directives.len(), 3);
        assert_eq!(result.source_map.len(), 2);
        assert_eq!(
            result.source_map[0]
                .origin
                .source_id
                .as_ref()
                .map(SourceId::as_str),
            Some("part")
        );
    }

    #[test]
    fn include_attributes_are_quote_aware_and_optional_missing_resources_are_notices() {
        let mut snapshot = ResourceSnapshot::default();
        snapshot.insert(
            "part.adoc",
            ResourceDocument {
                source_id: SourceId::new("part"),
                source: "// tag::one[]\none\n// end::one[]\n// tag::two[]\ntwo\n// end::two[]\n"
                    .to_owned(),
            },
        );

        let document = preprocess(
            "include::part.adoc[tags=\"one,two\"]\ninclude::missing.adoc[optional]\n",
            &snapshot,
            &PreprocessOptions::default(),
        )
        .expect("preprocess");

        assert_eq!(document.source, "one\ntwo\n");
        assert_eq!(document.directives.len(), 2);
        assert_eq!(document.directives[1].resource_source_id, None);
        assert_eq!(document.notices.len(), 1);
        assert_eq!(
            document.notices[0].kind,
            PreprocessNoticeKind::OptionalResourceMissing
        );
        assert_eq!(document.notices[0].target, "missing.adoc");

        assert_eq!(
            preprocess(
                "include::missing.adoc[optional,encoding=shift_jis]\n",
                &ResourceSnapshot::default(),
                &PreprocessOptions::default(),
            )
            .expect_err("optional must not suppress encoding failures")
            .kind,
            PreprocessErrorKind::UnsupportedEncoding
        );
        assert_eq!(
            preprocess(
                "include::../missing.adoc[optional]\n",
                &ResourceSnapshot::default(),
                &PreprocessOptions::default(),
            )
            .expect_err("optional must not suppress unsafe target failures")
            .kind,
            PreprocessErrorKind::UnsafeTarget
        );
    }

    #[test]
    fn cycles_limits_unsafe_targets_and_encoding_fail_before_parsing() {
        let mut snapshot = ResourceSnapshot::default();
        snapshot.insert(
            "cycle.adoc",
            ResourceDocument {
                source_id: SourceId::new("cycle"),
                source: "include::cycle.adoc[]\n".to_owned(),
            },
        );
        assert_eq!(
            preprocess(
                "include::cycle.adoc[]\n",
                &snapshot,
                &PreprocessOptions::default()
            )
            .expect_err("cycle")
            .kind,
            PreprocessErrorKind::IncludeCycle
        );
        assert_eq!(
            preprocess(
                "include::../outside.adoc[]\n",
                &snapshot,
                &PreprocessOptions::default()
            )
            .expect_err("unsafe")
            .kind,
            PreprocessErrorKind::UnsafeTarget
        );
        assert_eq!(
            preprocess(
                "include::cycle.adoc[encoding=shift_jis]\n",
                &snapshot,
                &PreprocessOptions::default()
            )
            .expect_err("encoding")
            .kind,
            PreprocessErrorKind::UnsupportedEncoding
        );
    }

    #[test]
    fn inline_and_expression_conditionals_follow_attribute_semantics() {
        let mut options = PreprocessOptions::default();
        options
            .attributes
            .insert("edition".to_owned(), "2".to_owned());
        options.attributes.insert("web".to_owned(), String::new());
        let source = concat!(
            "ifdef::web[inline]\n",
            "ifndef::print[also inline]\n",
            "ifeval::[{edition} >= 2]\n",
            "selected\n",
            "endif::[]\n",
            "\\include::literal.adoc[]\n",
        );
        let result = preprocess(source, &ResourceSnapshot::default(), &options).expect("result");
        assert_eq!(
            result.source,
            "inline\nalso inline\nselected\ninclude::literal.adoc[]\n"
        );
    }

    #[test]
    fn document_attributes_drive_includes_and_conditionals_in_read_order() {
        let mut snapshot = ResourceSnapshot::default();
        for (target, source_id, source) in [
            (
                "first.adoc",
                "first",
                include_str!("../../../fixtures/attributes/preprocessor-first.adoc"),
            ),
            (
                "second.adoc",
                "second",
                include_str!("../../../fixtures/attributes/preprocessor-second.adoc"),
            ),
            (
                "safe.adoc",
                "safe",
                include_str!("../../../fixtures/attributes/preprocessor-safe.adoc"),
            ),
            ("bad.adoc", "bad", "bad resource\n"),
        ] {
            snapshot.insert(
                target,
                ResourceDocument {
                    source_id: SourceId::new(source_id),
                    source: source.to_owned(),
                },
            );
        }
        let source = include_str!("../../../fixtures/attributes/preprocessor-read-order.adoc");

        let result =
            preprocess(source, &snapshot, &PreprocessOptions::default()).expect("preprocess");

        assert!(result.source.contains("second resource"));
        assert!(result.source.contains("included attribute is visible"));
        assert!(result.source.contains("safe resource"));
        assert!(result.source.contains("unset is visible"));
        assert!(!result.source.contains("bad resource"));
        assert_eq!(
            result
                .directives
                .iter()
                .filter(|directive| directive.kind == DirectiveKind::Include)
                .map(|directive| directive.target.as_str())
                .collect::<Vec<_>>(),
            ["first.adoc", "second.adoc", "safe.adoc"]
        );
    }

    #[test]
    fn multiline_locked_failed_and_delimited_definitions_follow_shared_rules() {
        let mut snapshot = ResourceSnapshot::default();
        for target in ["host.adoc", "folded- value.adoc"] {
            snapshot.insert(
                target,
                ResourceDocument {
                    source_id: SourceId::new(target),
                    source: format!("{target}\n"),
                },
            );
        }
        let source = "\
:locked: document
:part: folded- \\
 value
:literal: retained \\
include::missing.adoc[]
include::{locked}.adoc[]
include::{part}.adoc[]

:cycle: {cycle}
ifdef::cycle[]
cycle must stay hidden
endif::[]
----
:inside: visible
----
ifdef::inside[]
delimited attribute must stay hidden
endif::[]

:cycle: recovered
ifdef::cycle[]
recovered definition is visible
endif::[]
";
        let options = PreprocessOptions {
            attributes: BTreeMap::from([("locked".to_owned(), "host".to_owned())]),
            ..PreprocessOptions::default()
        };

        let result = preprocess(source, &snapshot, &options).expect("preprocess");

        assert!(result.source.contains("host.adoc"));
        assert!(result.source.contains("folded- value.adoc"));
        assert!(!result.source.contains("cycle must stay hidden"));
        assert!(
            !result
                .source
                .contains("delimited attribute must stay hidden")
        );
        assert!(result.source.contains("recovered definition is visible"));
        assert_eq!(
            result
                .directives
                .iter()
                .filter(|directive| directive.kind == DirectiveKind::Include)
                .map(|directive| directive.target.as_str())
                .collect::<Vec<_>>(),
            ["host.adoc", "folded- value.adoc"]
        );
    }

    #[test]
    fn base_uri_resolves_snapshot_keys_without_io() {
        let mut snapshot = ResourceSnapshot::default();
        snapshot.insert(
            "chapters/one.adoc",
            ResourceDocument {
                source_id: SourceId::new("one"),
                source: "chapter\n".to_owned(),
            },
        );
        let options = PreprocessOptions {
            base_uri: Some("chapters".to_owned()),
            ..PreprocessOptions::default()
        };
        let result = preprocess("include::one.adoc[]\n", &snapshot, &options).expect("result");
        assert_eq!(result.source, "chapter\n");
    }

    #[test]
    fn uri_base_preserves_snapshot_key_spelling() {
        assert_eq!(
            resolve_include_target("part.adoc", Some("file:///book")),
            "file:///book/part.adoc"
        );
    }

    #[test]
    fn nested_includes_resolve_from_the_including_resource() {
        let mut snapshot = ResourceSnapshot::default();
        snapshot.insert(
            "book/chapters/one.adoc",
            ResourceDocument {
                source_id: SourceId::new("one"),
                source: "include::section.adoc[]\n".to_owned(),
            },
        );
        snapshot.insert(
            "book/chapters/section.adoc",
            ResourceDocument {
                source_id: SourceId::new("section"),
                source: "nested\n".to_owned(),
            },
        );
        let options = PreprocessOptions {
            base_uri: Some("book/chapters".to_owned()),
            ..PreprocessOptions::default()
        };

        let result = preprocess("include::one.adoc[]\n", &snapshot, &options).expect("result");
        assert_eq!(result.source, "nested\n");
        assert_eq!(result.directives[1].target, "book/chapters/section.adoc");
    }

    #[test]
    fn include_discovery_is_io_free_and_ignores_escaped_or_incomplete_directives() {
        let requests = discover_includes(concat!(
            "include::one.adoc[tag=a]\n",
            "\\include::literal.adoc[]\n",
            "include::incomplete.adoc[\n",
            "ifdef::web[]\ninclude::conditional.adoc[]\nendif::[]\n",
        ))
        .expect("bounded source");

        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].target, "one.adoc");
        assert_eq!(requests[0].attributes, "tag=a");
        assert_eq!(requests[1].target, "conditional.adoc");
    }

    #[test]
    fn range_projection_preserves_identity_and_marks_transforms_conservatively() {
        let document = PreprocessedDocument::from_parts(
            "abcXYZ".to_owned(),
            vec![
                SourceMapSegment {
                    output_range: ExpandedRange::new(range(0, 3)),
                    origin: SourceOrigin {
                        source_id: Some(SourceId::new("root")),
                        range: OriginRange::new(range(10, 13)),
                    },
                    mapping: SourceMapping::Identity,
                },
                SourceMapSegment {
                    output_range: ExpandedRange::new(range(3, 6)),
                    origin: SourceOrigin {
                        source_id: Some(SourceId::new("included")),
                        range: OriginRange::new(range(20, 28)),
                    },
                    mapping: SourceMapping::WholeOrigin,
                },
            ],
            Vec::new(),
            Vec::new(),
        )
        .expect("valid source map");

        assert_eq!(
            document.origins_for_range(ExpandedRange::new(range(1, 5))),
            vec![
                SourceOrigin {
                    source_id: Some(SourceId::new("root")),
                    range: OriginRange::new(range(11, 13)),
                },
                SourceOrigin {
                    source_id: Some(SourceId::new("included")),
                    range: OriginRange::new(range(20, 28)),
                },
            ]
        );
        assert_eq!(
            document.origins_for_range(ExpandedRange::new(range(2, 2))),
            vec![SourceOrigin {
                source_id: Some(SourceId::new("root")),
                range: OriginRange::new(range(12, 12)),
            }]
        );
        assert_eq!(
            document.origins_for_range(ExpandedRange::new(range(3, 3))),
            vec![SourceOrigin {
                source_id: Some(SourceId::new("included")),
                range: OriginRange::new(range(20, 28)),
            }]
        );
    }

    #[test]
    fn analysis_projection_maps_reference_resource_and_symbol_targets() {
        let mut snapshot = ResourceSnapshot::default();
        snapshot.insert(
            "part.adoc",
            ResourceDocument {
                source_id: SourceId::new("part"),
                source: "== Included\nSee xref:other.adoc#target[] and image::cover.png[].\n"
                    .to_owned(),
            },
        );
        let engine = Engine::new(crate::core::AnalysisOptions::default());
        let analysis = preprocess_and_analyze(
            &engine,
            "include::part.adoc[]\n",
            &snapshot,
            &PreprocessOptions {
                source_id: Some(SourceId::new("root")),
                ..PreprocessOptions::default()
            },
        )
        .expect("analysis");
        let projection = analysis
            .project_origins(ProjectionLimits::default())
            .expect("projection");

        assert_eq!(projection.symbols.len(), 1);
        assert_eq!(projection.references.len(), 1);
        assert_eq!(projection.resources.len(), 1);
        assert_eq!(
            projection.references[0].target_origins[0]
                .source_id
                .as_ref()
                .map(SourceId::as_str),
            Some("part")
        );
        assert_eq!(
            projection.resources[0].target_origins[0]
                .source_id
                .as_ref()
                .map(SourceId::as_str),
            Some("part")
        );
    }

    #[test]
    fn analysis_projection_maps_included_body_attribute_occurrences() {
        let included = include_str!("../../../fixtures/attributes/body-set-unset.adoc");
        let mut snapshot = ResourceSnapshot::default();
        snapshot.insert(
            "attributes.adoc",
            ResourceDocument {
                source_id: SourceId::new("included-attributes"),
                source: included.to_owned(),
            },
        );
        let engine = Engine::new(crate::core::AnalysisOptions::default());
        let analysis = preprocess_and_analyze(
            &engine,
            "include::attributes.adoc[]\n",
            &snapshot,
            &PreprocessOptions {
                source_id: Some(SourceId::new("root")),
                ..PreprocessOptions::default()
            },
        )
        .expect("analysis");
        let projection = analysis
            .project_origins(ProjectionLimits::default())
            .expect("projection");

        assert_eq!(projection.attribute_occurrences.len(), 2);
        for attribute in &projection.attribute_occurrences {
            assert_eq!(
                attribute.origins[0]
                    .source_id
                    .as_ref()
                    .map(SourceId::as_str),
                Some("included-attributes")
            );
            assert_eq!(
                attribute.name_origins[0]
                    .source_id
                    .as_ref()
                    .map(SourceId::as_str),
                Some("included-attributes")
            );
            assert_eq!(
                attribute.value_origins[0]
                    .source_id
                    .as_ref()
                    .map(SourceId::as_str),
                Some("included-attributes")
            );
        }
        let theme = &projection.attribute_occurrences[0];
        assert_eq!(
            theme.origins[0].range.text_range(),
            text_range_in(included, ":theme: dark\n")
        );
        assert_eq!(
            theme.name_origins[0].range.text_range(),
            text_range_in(included, "theme")
        );
        assert_eq!(
            theme.value_origins[0].range.text_range(),
            text_range_in(included, "dark")
        );
    }

    #[test]
    fn analysis_projection_connects_attribute_references_to_included_bindings() {
        let included = ":shared: included\n";
        let root = "include::attributes.adoc[]\n\n{shared}\n";
        let mut snapshot = ResourceSnapshot::default();
        snapshot.insert(
            "attributes.adoc",
            ResourceDocument {
                source_id: SourceId::new("included-attributes"),
                source: included.to_owned(),
            },
        );
        let analysis = preprocess_and_analyze(
            &Engine::new(crate::core::AnalysisOptions::default()),
            root,
            &snapshot,
            &PreprocessOptions {
                source_id: Some(SourceId::new("root")),
                ..PreprocessOptions::default()
            },
        )
        .expect("analysis");
        let projection = analysis
            .project_origins(ProjectionLimits::default())
            .expect("projection");

        assert_eq!(projection.attribute_bindings.len(), 1);
        assert_eq!(projection.attribute_references.len(), 1);
        let binding = &projection.attribute_bindings[0];
        let reference = &projection.attribute_references[0];
        assert_eq!(reference.value.binding_id, Some(binding.value.id()));
        assert_eq!(reference.value.value, Ok(Some("included".to_owned())));
        assert_eq!(
            binding.name_origins[0]
                .source_id
                .as_ref()
                .map(SourceId::as_str),
            Some("included-attributes")
        );
        assert_eq!(
            reference.name_origins[0]
                .source_id
                .as_ref()
                .map(SourceId::as_str),
            Some("root")
        );
        assert_eq!(
            binding.name_origins[0].range.text_range(),
            text_range_in(included, "shared")
        );
        assert_eq!(
            reference.name_origins[0].range.text_range(),
            text_range_in(root, "shared")
        );
    }

    #[test]
    fn analysis_projection_preserves_each_included_attribute_value_line() {
        let included = include_str!("../../../fixtures/attributes/multiline-soft-hard.adoc");
        let mut snapshot = ResourceSnapshot::default();
        snapshot.insert(
            "multiline.adoc",
            ResourceDocument {
                source_id: SourceId::new("included-multiline"),
                source: included.to_owned(),
            },
        );
        let engine = Engine::new(crate::core::AnalysisOptions::default());
        let analysis = preprocess_and_analyze(
            &engine,
            "include::multiline.adoc[]\n",
            &snapshot,
            &PreprocessOptions {
                source_id: Some(SourceId::new("root")),
                ..PreprocessOptions::default()
            },
        )
        .expect("analysis");
        let projection = analysis
            .project_origins(ProjectionLimits::default())
            .expect("projection");

        let soft = &projection.attribute_occurrences[0];
        assert_eq!(
            soft.value.value.folded_text,
            "first line 日本語🙂 third line"
        );
        assert_eq!(soft.value_lines.len(), 3);
        for line in &soft.value_lines {
            for origins in [&line.origins, &line.content_origins, &line.ending_origins] {
                assert_eq!(origins.len(), 1);
                assert_eq!(
                    origins[0].source_id.as_ref().map(SourceId::as_str),
                    Some("included-multiline")
                );
            }
        }
        assert_eq!(
            soft.value_lines
                .iter()
                .map(|line| {
                    let range = line.content_origins[0].range.text_range();
                    &included[range.start().to_usize()..range.end().to_usize()]
                })
                .collect::<Vec<_>>(),
            ["first line", "日本語🙂", "third line"]
        );
    }

    #[test]
    fn empty_attribute_value_at_an_include_boundary_projects_to_the_include() {
        let included = include_str!("../../../fixtures/attributes/include-empty-no-newline.adoc");
        assert!(!included.ends_with('\n'));
        let mut snapshot = ResourceSnapshot::default();
        snapshot.insert(
            "empty.adoc",
            ResourceDocument {
                source_id: SourceId::new("empty-include"),
                source: included.to_owned(),
            },
        );
        let analysis = preprocess_and_analyze(
            &Engine::new(crate::core::AnalysisOptions::default()),
            "include::empty.adoc[]\n\nBody\n",
            &snapshot,
            &PreprocessOptions {
                source_id: Some(SourceId::new("root")),
                ..PreprocessOptions::default()
            },
        )
        .expect("analysis");
        let projection = analysis
            .project_origins(ProjectionLimits::default())
            .expect("projection");

        assert_eq!(projection.attribute_occurrences.len(), 1);
        let attribute = &projection.attribute_occurrences[0];
        assert!(attribute.value.value.source_range.is_empty());
        assert_eq!(
            attribute.value_origins,
            vec![SourceOrigin {
                source_id: Some(SourceId::new("empty-include")),
                range: OriginRange::new(range(included.len(), included.len())),
            }]
        );
        assert_eq!(
            attribute.origins.len(),
            2,
            "the line ending originates in the root segment"
        );
    }

    fn text_range_in(source: &str, needle: &str) -> TextRange {
        let start = source.find(needle).expect("fixture contains needle");
        range(start, start + needle.len())
    }

    #[test]
    fn source_map_and_projection_limits_fail_explicitly() {
        let source_map_error = preprocess(
            "one\ntwo\n",
            &ResourceSnapshot::default(),
            &PreprocessOptions {
                max_source_map_segments: 1,
                ..PreprocessOptions::default()
            },
        )
        .expect_err("source map limit");
        assert_eq!(source_map_error.kind, PreprocessErrorKind::SourceMapLimit);

        let engine = Engine::new(crate::core::AnalysisOptions::default());
        let analysis = preprocess_and_analyze(
            &engine,
            "= Title\n\n== Section\n",
            &ResourceSnapshot::default(),
            &PreprocessOptions::default(),
        )
        .expect("analysis");
        let error = analysis
            .project_origins(ProjectionLimits {
                max_origin_segments: 1,
            })
            .expect_err("projection limit");
        assert_eq!(error.limit, 1);
        assert!(error.actual > 1);
    }

    #[test]
    fn source_map_constructor_rejects_unsorted_overlap_and_out_of_bounds_segments() {
        let segment = |start, end| SourceMapSegment {
            output_range: ExpandedRange::new(range(start, end)),
            origin: SourceOrigin {
                source_id: None,
                range: OriginRange::new(range(start, end)),
            },
            mapping: SourceMapping::Identity,
        };
        assert!(
            PreprocessedDocument::from_parts(
                "abcd".to_owned(),
                vec![segment(2, 4), segment(1, 2)],
                Vec::new(),
                Vec::new(),
            )
            .is_err()
        );
        assert!(
            PreprocessedDocument::from_parts(
                "abcd".to_owned(),
                vec![segment(0, 3), segment(2, 4)],
                Vec::new(),
                Vec::new(),
            )
            .is_err()
        );
        assert!(
            PreprocessedDocument::from_parts(
                "abcd".to_owned(),
                vec![segment(0, 5)],
                Vec::new(),
                Vec::new(),
            )
            .is_err()
        );
    }

    #[test]
    fn disabled_include_capability_preserves_syntax_without_resolving() {
        let source = "include::missing.adoc[]\n";
        let document = preprocess(
            source,
            &ResourceSnapshot::default(),
            &PreprocessOptions {
                enable_includes: false,
                ..PreprocessOptions::default()
            },
        )
        .expect("disabled include does not require a resource");

        assert_eq!(document.source, source);
        assert_eq!(document.directives.len(), 1);
        assert_eq!(document.directives[0].kind, DirectiveKind::Include);
        assert!(document.directives[0].resource_source_id.is_none());
    }
}
