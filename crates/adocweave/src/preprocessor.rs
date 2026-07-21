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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreprocessOptions {
    pub source_id: Option<SourceId>,
    pub base_uri: Option<String>,
    pub safe_mode: SafeMode,
    pub allowed_schemes: BTreeSet<String>,
    pub attributes: BTreeMap<String, String>,
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
    pub target: String,
    pub target_range: TextRange,
    /// Definition target for an include; absent for conditionals.
    pub resource_source_id: Option<SourceId>,
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
}

impl PreprocessedDocument {
    fn from_parts(
        source: String,
        source_map: Vec<SourceMapSegment>,
        directives: Vec<Directive>,
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
            if let Some(previous) = origins.last_mut()
                && previous.source_id == origin.source_id
                && previous.range.end() == origin.range.start()
            {
                previous.range = OriginRange::new(
                    TextRange::new(previous.range.start(), origin.range.end())
                        .expect("merged source range is ordered"),
                );
            } else {
                origins.push(origin);
            }
        }
        origins
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
pub struct ProjectedResource {
    pub value: ResourceReference,
    pub origins: Vec<SourceOrigin>,
    pub target_origins: Vec<SourceOrigin>,
}

/// All editor-facing facts from an expanded analysis, projected to original sources.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnalysisProjection {
    pub directives: Vec<Directive>,
    pub diagnostics: Vec<ProjectedDiagnostic>,
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
        let references = self
            .analysis
            .references()
            .into_iter()
            .map(|value| {
                Ok(ProjectedReference {
                    origins: project(value.range)?,
                    target_origins: project(value.target_range)?,
                    value: value.clone(),
                })
            })
            .collect::<Result<Vec<_>, ProjectionError>>()?;
        let resources = self
            .analysis
            .resources()
            .into_iter()
            .map(|value| {
                Ok(ProjectedResource {
                    origins: project(value.range)?,
                    target_origins: project(value.target_range)?,
                    value,
                })
            })
            .collect::<Result<Vec<_>, ProjectionError>>()?;
        let symbols = crate::document::document_symbols(self.analysis.ast())
            .into_iter()
            .map(|symbol| project_symbol(symbol, &mut project))
            .collect::<Result<Vec<_>, ProjectionError>>()?;
        Ok(AnalysisProjection {
            directives: self.document.directives.clone(),
            diagnostics,
            references,
            resources,
            symbols,
        })
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
    let mut context = Context {
        snapshot,
        options,
        output: String::new(),
        source_map: Vec::new(),
        directives: Vec::new(),
        active: Vec::new(),
        expanded_nodes: 0,
        includes: 0,
    };
    context.expand(
        source,
        options.source_id.clone(),
        0,
        options.base_uri.as_deref(),
    )?;
    PreprocessedDocument::from_parts(context.output, context.source_map, context.directives)
        .map_err(|_| PreprocessError {
            kind: PreprocessErrorKind::InternalInvariant,
            source_id: options.source_id.clone(),
            range: TextRange::new(TextSize::ZERO, TextSize::ZERO).expect("zero range is ordered"),
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
    active: Vec<String>,
    expanded_nodes: u64,
    includes: u64,
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
        let expanded_target = expand_attributes(&include.target, &self.options.attributes);
        let target = resolve_target(&expanded_target, base_uri);
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
        let document = self.snapshot.get(&target).ok_or_else(|| {
            error(
                PreprocessErrorKind::MissingResource,
                source_id.clone(),
                range,
                format!("resource snapshot does not contain {target}"),
            )
        })?;
        self.directives.push(Directive {
            kind: DirectiveKind::Include,
            source_id: source_id.clone(),
            range,
            target: target.clone(),
            target_range: relative_range(range, include.target_start, include.target_end),
            resource_source_id: Some(document.source_id.clone()),
        });
        let attributes = parse_attributes(&include.attributes);
        if let Some(encoding) = attributes.get("encoding")
            && !encoding.eq_ignore_ascii_case("utf-8")
            && !encoding.eq_ignore_ascii_case("utf8")
        {
            return Err(error(
                PreprocessErrorKind::UnsupportedEncoding,
                Some(document.source_id.clone()),
                zero_range(),
                "resource snapshots contain UTF-8 text only",
            ));
        }
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
        let mut conditions = Vec::<bool>::new();
        for line in lines {
            let content = line.text.trim_end_matches(['\r', '\n']);
            let enabled = conditions.iter().all(|condition| *condition);
            if let Some(directive) = conditional_directive(content) {
                self.bump_node(source_id.clone(), line.range)?;
                self.directives.push(Directive {
                    kind: directive.kind,
                    source_id: source_id.clone(),
                    range: line.range,
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
                                &self.options.attributes,
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
                        }
                    }
                    DirectiveKind::Ifdef => conditions.push(
                        enabled
                            && conditional_attribute(
                                &directive.target,
                                &self.options.attributes,
                                true,
                            ),
                    ),
                    DirectiveKind::Ifndef => conditions.push(
                        enabled
                            && conditional_attribute(
                                &directive.target,
                                &self.options.attributes,
                                false,
                            ),
                    ),
                    DirectiveKind::Ifeval => conditions.push(
                        enabled
                            && evaluate_expression(&expand_attributes(
                                &directive.attributes,
                                &self.options.attributes,
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
                if let Some(include) = include_directive(content) {
                    self.expand_include(include, source_id.clone(), line.range, depth, base_uri)?;
                } else if let Some(literal) = escaped_directive(content) {
                    let ending = &line.text[content.len()..];
                    self.append(
                        &format!("{literal}{ending}"),
                        source_id.clone(),
                        line.range,
                        SourceMapping::WholeOrigin,
                    )?;
                } else {
                    self.bump_node(source_id.clone(), line.range)?;
                    self.append(&line.text, source_id.clone(), line.range, line.mapping)?;
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

fn parse_attributes(value: &str) -> BTreeMap<String, String> {
    value
        .split(',')
        .filter_map(|item| item.trim().split_once('='))
        .map(|(name, value)| {
            (
                name.trim().to_owned(),
                value.trim().trim_matches(['\'', '"']).to_owned(),
            )
        })
        .collect()
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
        || target.split('/').any(|segment| segment == "..")
    {
        return Err("unsafe include target");
    }
    if let Some((scheme, _)) = target.split_once(':') {
        if options.safe_mode == SafeMode::Secure
            || !options
                .allowed_schemes
                .contains(&scheme.to_ascii_lowercase())
        {
            return Err("include target scheme is not allowed");
        }
    }
    Ok(())
}

fn resolve_target(target: &str, base_uri: Option<&str>) -> String {
    if target.contains(':') || target.starts_with('/') || target.starts_with('\\') {
        return target.to_owned();
    }
    let Some(base_uri) = base_uri.filter(|base| !base.is_empty()) else {
        return target.to_owned();
    };
    format!("{}/{target}", base_uri.trim_end_matches('/'))
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
        let engine = Engine::new(crate::core::ParseOptions::default());
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

        let engine = Engine::new(crate::core::ParseOptions::default());
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
            )
            .is_err()
        );
        assert!(
            PreprocessedDocument::from_parts(
                "abcd".to_owned(),
                vec![segment(0, 3), segment(2, 4)],
                Vec::new(),
            )
            .is_err()
        );
        assert!(
            PreprocessedDocument::from_parts("abcd".to_owned(), vec![segment(0, 5)], Vec::new(),)
                .is_err()
        );
    }
}
