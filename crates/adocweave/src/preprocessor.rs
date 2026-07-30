//! Pure preprocessing over caller-provided resource snapshots.

mod directive;
mod expansion;
mod projection;
mod source_map;

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::sync::Arc;

use crate::cancellation::CancellationCheckpoint;
use crate::core::{Analysis, CancellationCheck, Engine, NeverCancel, ParseError, SourceId};
use crate::source::PositionError;
use crate::source::{TextRange, TextSize};
use crate::substitution::AttributeExpansionLimits;
use directive::{ConditionalTransition, ParsedDirective, RecognizedDirective};
use expansion::{ExpansionLimit, ExpansionState, IncludeFrame};
pub use projection::{
    AnalysisProjection, Originated, ProjectedAttributeBinding, ProjectedAttributeReference,
    ProjectedDiagnostic, ProjectedDocumentAttribute, ProjectedDocumentAttributeValueLine,
    ProjectedDocumentSymbol, ProjectedFix, ProjectedLocalTarget, ProjectedReference,
    ProjectedResource, ProjectionError, ProjectionFailure, ProjectionLimits,
};

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
    pub source: Arc<str>,
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
    pub attributes: crate::attributes::ExternalAttributes,
    /// Expands include directives only from the caller-provided snapshot.
    pub enable_includes: bool,
    pub max_include_depth: u32,
    pub max_includes: u32,
    pub max_total_bytes: u32,
    pub max_expanded_nodes: u32,
    pub max_source_map_segments: u32,
    pub max_attribute_expansion_depth: u32,
    pub max_attribute_expansion_bytes: u32,
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
            max_attribute_expansion_depth: 32,
            max_attribute_expansion_bytes: 1024 * 1024,
        }
    }
}

/// A validated, immutable configuration for preprocessing followed by analysis.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveProcessingOptions {
    analysis: crate::core::AnalysisOptions,
    preprocess: PreprocessOptions,
}

impl EffectiveProcessingOptions {
    /// Validates that settings consumed by both stages have one effective value.
    pub fn new(
        analysis: crate::core::AnalysisOptions,
        preprocess: PreprocessOptions,
    ) -> Result<Self, ProcessingOptionsError> {
        if analysis.attributes != preprocess.attributes {
            return Err(ProcessingOptionsError::ExternalAttributes);
        }
        if analysis.syntax.limits.max_attribute_expansion_depth
            != preprocess.max_attribute_expansion_depth
        {
            return Err(ProcessingOptionsError::AttributeExpansionDepth);
        }
        if analysis.syntax.limits.max_attribute_expansion_bytes
            != preprocess.max_attribute_expansion_bytes
        {
            return Err(ProcessingOptionsError::AttributeExpansionBytes);
        }
        Ok(Self {
            analysis,
            preprocess,
        })
    }

    /// Returns the analysis settings in this effective contract.
    pub const fn analysis(&self) -> &crate::core::AnalysisOptions {
        &self.analysis
    }

    /// Returns the preprocessing settings in this effective contract.
    pub const fn preprocess(&self) -> &PreprocessOptions {
        &self.preprocess
    }

    /// Returns the same effective settings with one source identity.
    pub fn with_source_id(mut self, source_id: Option<SourceId>) -> Self {
        self.preprocess.source_id = source_id;
        self
    }
}

/// Inconsistent values supplied through a compatibility processing entry point.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessingOptionsError {
    /// External attributes differ between analysis and preprocessing.
    ExternalAttributes,
    /// Attribute expansion depth limits differ between stages.
    AttributeExpansionDepth,
    /// Attribute expansion byte limits differ between stages.
    AttributeExpansionBytes,
}

impl ProcessingOptionsError {
    /// Returns the stable kebab-case code.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExternalAttributes => "external-attributes-mismatch",
            Self::AttributeExpansionDepth => "attribute-expansion-depth-mismatch",
            Self::AttributeExpansionBytes => "attribute-expansion-bytes-mismatch",
        }
    }
}

impl fmt::Display for ProcessingOptionsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ExternalAttributes => {
                "analysis and preprocessing external attributes do not match"
            }
            Self::AttributeExpansionDepth => {
                "analysis and preprocessing attribute expansion depth limits do not match"
            }
            Self::AttributeExpansionBytes => {
                "analysis and preprocessing attribute expansion byte limits do not match"
            }
        })
    }
}

impl Error for ProcessingOptionsError {}

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

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SourceMapInvariantError;

/// Analysis paired with the source map used to build it.
#[derive(Debug)]
pub struct PreprocessedAnalysis {
    pub document: PreprocessedDocument,
    pub analysis: Analysis,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PreprocessedAnalysisError {
    /// Combined processing settings are inconsistent.
    Options(ProcessingOptionsError),
    Preprocess(PreprocessError),
    Parse(ParseError),
    Cancelled,
}

impl fmt::Display for PreprocessedAnalysisError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Options(error) => error.fmt(formatter),
            Self::Preprocess(error) => error.fmt(formatter),
            Self::Parse(error) => error.fmt(formatter),
            Self::Cancelled => formatter.write_str("processing was cancelled"),
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
    preprocess_and_analyze_cancellable(engine, source, snapshot, options, &NeverCancel)
}

/// Expands and analyzes caller-provided input with cooperative cancellation.
pub fn preprocess_and_analyze_cancellable(
    engine: &Engine,
    source: &str,
    snapshot: &ResourceSnapshot,
    options: &PreprocessOptions,
    cancellation: &dyn CancellationCheck,
) -> Result<PreprocessedAnalysis, PreprocessedAnalysisError> {
    let options = EffectiveProcessingOptions::new(engine.options().clone(), options.clone())
        .map_err(PreprocessedAnalysisError::Options)?;
    preprocess_and_analyze_cancellable_with_options(source, snapshot, &options, cancellation)
}

/// Expands and analyzes with one previously validated effective configuration.
pub fn preprocess_and_analyze_with_options(
    source: &str,
    snapshot: &ResourceSnapshot,
    options: &EffectiveProcessingOptions,
) -> Result<PreprocessedAnalysis, PreprocessedAnalysisError> {
    preprocess_and_analyze_cancellable_with_options(source, snapshot, options, &NeverCancel)
}

/// Expands and analyzes with validated settings and cooperative cancellation.
pub fn preprocess_and_analyze_cancellable_with_options(
    source: &str,
    snapshot: &ResourceSnapshot,
    options: &EffectiveProcessingOptions,
    cancellation: &dyn CancellationCheck,
) -> Result<PreprocessedAnalysis, PreprocessedAnalysisError> {
    let document = preprocess_cancellable(source, snapshot, options.preprocess(), cancellation)
        .map_err(|failure| match failure {
            PreprocessFailure::Error(error) => PreprocessedAnalysisError::Preprocess(error),
            PreprocessFailure::Cancelled => PreprocessedAnalysisError::Cancelled,
        })?;
    let analysis = Engine::new(options.analysis().clone())
        .analyze_cancellable_with_source_id(
            options.preprocess().source_id.as_ref(),
            &document.source,
            cancellation,
        )
        .map_err(|error| {
            if error == ParseError::Cancelled {
                PreprocessedAnalysisError::Cancelled
            } else {
                PreprocessedAnalysisError::Parse(error)
            }
        })?;
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PreprocessFailure {
    Error(PreprocessError),
    Cancelled,
}

impl fmt::Display for PreprocessFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Error(error) => error.fmt(formatter),
            Self::Cancelled => formatter.write_str("preprocessing was cancelled"),
        }
    }
}

impl Error for PreprocessFailure {}

impl From<PreprocessError> for PreprocessFailure {
    fn from(error: PreprocessError) -> Self {
        Self::Error(error)
    }
}

pub fn preprocess(
    source: &str,
    snapshot: &ResourceSnapshot,
    options: &PreprocessOptions,
) -> Result<PreprocessedDocument, PreprocessError> {
    match preprocess_cancellable(source, snapshot, options, &NeverCancel) {
        Ok(document) => Ok(document),
        Err(PreprocessFailure::Error(error)) => Err(error),
        Err(PreprocessFailure::Cancelled) => {
            unreachable!("NeverCancel cannot cancel preprocessing")
        }
    }
}

/// Expands a caller-provided snapshot with cooperative cancellation.
pub fn preprocess_cancellable(
    source: &str,
    snapshot: &ResourceSnapshot,
    options: &PreprocessOptions,
    cancellation: &dyn CancellationCheck,
) -> Result<PreprocessedDocument, PreprocessFailure> {
    if cancellation.is_cancelled() {
        return Err(PreprocessFailure::Cancelled);
    }
    let mut context = Context {
        snapshot,
        options,
        cancellation,
        checkpoint: CancellationCheckpoint::new(cancellation),
        source_map: source_map::SourceMapBuilder::new(
            options.max_total_bytes,
            options.max_source_map_segments,
        ),
        directives: Vec::new(),
        notices: Vec::new(),
        state: ExpansionState::new(
            &options.attributes,
            AttributeExpansionLimits {
                max_depth: options.max_attribute_expansion_depth,
                max_bytes: options.max_attribute_expansion_bytes,
            },
        ),
    };
    context.expand(
        source,
        IncludeFrame::root(options.source_id.clone(), options.base_uri.as_deref()),
    )?;
    if cancellation.is_cancelled() {
        return Err(PreprocessFailure::Cancelled);
    }
    let Context {
        source_map,
        directives,
        notices,
        mut checkpoint,
        ..
    } = context;
    let document = source_map
        .finish_cancellable(directives, notices, &mut checkpoint)
        .map_err(|failure| match failure {
            source_map::SourceMapFinishError::Cancelled => PreprocessFailure::Cancelled,
            source_map::SourceMapFinishError::Invariant => {
                PreprocessFailure::Error(PreprocessError {
                    kind: PreprocessErrorKind::InternalInvariant,
                    source_id: options.source_id.clone(),
                    range: TextRange::new(TextSize::ZERO, TextSize::ZERO)
                        .expect("zero range is ordered"),
                    requested_target: None,
                    target: None,
                    message:
                        "source map segments are unsorted, overlapping, or outside expanded source"
                            .to_owned(),
                })
            }
        })?;
    if cancellation.is_cancelled() {
        Err(PreprocessFailure::Cancelled)
    } else {
        Ok(document)
    }
}

struct Context<'a> {
    snapshot: &'a ResourceSnapshot,
    options: &'a PreprocessOptions,
    cancellation: &'a dyn CancellationCheck,
    checkpoint: CancellationCheckpoint<'a>,
    source_map: source_map::SourceMapBuilder,
    directives: Vec<Directive>,
    notices: Vec<PreprocessNotice>,
    state: ExpansionState,
}

impl Context<'_> {
    fn expand(&mut self, source: &str, frame: IncludeFrame) -> Result<(), PreprocessFailure> {
        let mut offset = 0;
        let mut lines = Vec::new();
        for line in source.split_inclusive('\n') {
            let start = offset;
            offset += line.len();
            let line_range = range(start, offset);
            self.check_cancelled()?;
            lines.push(SelectedLine {
                text: line.to_owned(),
                range: line_range,
                mapping: SourceMapping::Identity,
            });
        }
        self.expand_selected(lines, frame)
    }

    fn expand_include(
        &mut self,
        include: ParsedDirective,
        frame: &IncludeFrame,
        range: TextRange,
    ) -> Result<(), PreprocessFailure> {
        let source_id = frame.source_id();
        if frame.depth() >= self.options.max_include_depth {
            return Err(error(
                PreprocessErrorKind::DepthLimit,
                source_id.clone(),
                range,
                "include depth limit exceeded",
            )
            .into());
        }
        if self
            .state
            .register_include(self.options.max_includes)
            .is_err()
        {
            return Err(error(
                PreprocessErrorKind::IncludeLimit,
                source_id,
                range,
                "include count limit exceeded",
            )
            .into());
        }
        self.bump_node(source_id.clone(), range)?;
        let expanded_target =
            directive::expand_attributes(&include.target, self.state.attributes());
        let target = resolve_include_target(&expanded_target, frame.base_uri());
        validate_target(&target, self.options).map_err(|message| {
            error(
                PreprocessErrorKind::UnsafeTarget,
                source_id.clone(),
                range,
                message,
            )
        })?;
        if frame.contains_target(&target) {
            return Err(error(
                PreprocessErrorKind::IncludeCycle,
                source_id,
                range,
                "include cycle detected",
            )
            .into());
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
            )
            .into());
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
            }
            .into());
        };
        let selected = select_lines(&document.source, &attributes, self.cancellation)
            .map_err(|_| PreprocessFailure::Cancelled)?;
        let remaining_bytes = self.source_map.remaining_bytes();
        let transformed =
            transform_lines(selected, &attributes, remaining_bytes, self.cancellation).map_err(
                |failure| match failure {
                    TransformFailure::Cancelled => PreprocessFailure::Cancelled,
                    TransformFailure::ByteLimit => error(
                        PreprocessErrorKind::ByteLimit,
                        source_id.clone(),
                        range,
                        "preprocessor byte limit exceeded",
                    )
                    .into(),
                },
            )?;
        let child = frame.child(
            target.clone(),
            document.source_id.clone(),
            target_base(&target),
        );
        self.expand_selected(transformed, child)
    }

    fn expand_selected(
        &mut self,
        lines: Vec<SelectedLine>,
        frame: IncludeFrame,
    ) -> Result<(), PreprocessFailure> {
        let source_id = frame.source_id();
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
            )
            .into());
        }
        let mut conditions = Vec::<bool>::new();
        let mut attribute_value_through = None;
        for (line_index, line) in lines.into_iter().enumerate() {
            self.check_cancelled()?;
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
            match directive::recognize(content) {
                RecognizedDirective::Conditional(directive) => {
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
                    match directive::transition(&directive, enabled, self.state.attributes()) {
                        ConditionalTransition::Inline { selected } => {
                            if selected {
                                let ending = &line.text[content.len()..];
                                self.append(
                                    &format!("{}{ending}", directive.attributes),
                                    source_id.clone(),
                                    line.range,
                                    SourceMapping::WholeOrigin,
                                )?;
                                self.state.finish_directive_output();
                            }
                        }
                        ConditionalTransition::Open { enabled: condition } => {
                            conditions.push(condition)
                        }
                        ConditionalTransition::Close => {
                            if conditions.pop().is_none() {
                                return Err(error(
                                    PreprocessErrorKind::InvalidDirective,
                                    source_id,
                                    line.range,
                                    "endif has no matching conditional",
                                )
                                .into());
                            }
                        }
                    }
                }
                RecognizedDirective::Include(include) if enabled => {
                    if self.options.enable_includes {
                        self.expand_include(include, &frame, line.range)?;
                    } else {
                        self.bump_node(source_id.clone(), line.range)?;
                        let authored_target =
                            directive::expand_attributes(&include.target, self.state.attributes());
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
                        self.state.finish_directive_output();
                    }
                }
                RecognizedDirective::Escaped(literal) if enabled => {
                    let ending = &line.text[content.len()..];
                    self.append(
                        &format!("{literal}{ending}"),
                        source_id.clone(),
                        line.range,
                        SourceMapping::WholeOrigin,
                    )?;
                    self.state.finish_directive_output();
                }
                RecognizedDirective::Text if enabled => {
                    let delimiter = self.state.observe_delimiter(content);
                    let mut document_attribute = false;
                    self.bump_node(source_id.clone(), line.range)?;
                    if self.state.accepts_attribute(delimiter)
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
                                self.cancellation.is_cancelled()
                            })
                            .map_err(|failure| match failure {
                                crate::parser_support::ParseFailure::Cancelled => {
                                    PreprocessFailure::Cancelled
                                }
                                crate::parser_support::ParseFailure::Position(_)
                                | crate::parser_support::ParseFailure::Budget(_)
                                | crate::parser_support::ParseFailure::InternalInvariant => error(
                                    PreprocessErrorKind::InternalInvariant,
                                    source_id.clone(),
                                    line.range,
                                    "attribute preprocessing failed",
                                )
                                .into(),
                            })?
                    {
                        self.state.apply_attribute(&occurrence);
                        document_attribute = true;
                        if last_line > line_index {
                            attribute_value_through = Some(last_line);
                        }
                    }
                    self.append(&line.text, source_id.clone(), line.range, line.mapping)?;
                    self.state.finish_line(document_attribute, content);
                }
                RecognizedDirective::Include(_)
                | RecognizedDirective::Escaped(_)
                | RecognizedDirective::Text => {}
            }
        }
        if !conditions.is_empty() {
            return Err(error(
                PreprocessErrorKind::UnclosedConditional,
                source_id,
                zero_range(),
                "conditional directive is not closed",
            )
            .into());
        }
        Ok(())
    }

    fn check_cancelled(&mut self) -> Result<(), PreprocessFailure> {
        if self.checkpoint.is_cancelled() {
            Err(PreprocessFailure::Cancelled)
        } else {
            Ok(())
        }
    }

    fn bump_node(
        &mut self,
        source_id: Option<SourceId>,
        range: TextRange,
    ) -> Result<(), PreprocessFailure> {
        if self.state.register_node(self.options.max_expanded_nodes) == Err(ExpansionLimit::Nodes) {
            return Err(error(
                PreprocessErrorKind::NodeLimit,
                source_id,
                range,
                "preprocessor node limit exceeded",
            )
            .into());
        }
        Ok(())
    }

    fn append(
        &mut self,
        value: &str,
        source_id: Option<SourceId>,
        origin_range: TextRange,
        mapping: SourceMapping,
    ) -> Result<(), PreprocessFailure> {
        self.source_map
            .append(value, source_id.clone(), origin_range, mapping)
            .map_err(|build_error| match build_error {
                source_map::SourceMapBuildError::ByteLimit => error(
                    PreprocessErrorKind::ByteLimit,
                    source_id,
                    origin_range,
                    "preprocessor byte limit exceeded",
                ),
                source_map::SourceMapBuildError::SegmentLimit => error(
                    PreprocessErrorKind::SourceMapLimit,
                    source_id,
                    origin_range,
                    "source map segment limit exceeded",
                ),
            })
            .map_err(PreprocessFailure::from)
    }
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
        if let RecognizedDirective::Include(include) = directive::recognize(content) {
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

fn select_lines(
    source: &str,
    attributes: &BTreeMap<String, String>,
    cancellation: &dyn CancellationCheck,
) -> Result<Vec<SelectedLine>, TextRange> {
    let mut checkpoint = CancellationCheckpoint::new(cancellation);
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
        .map(|value| parse_line_selection(value, cancellation))
        .transpose()?;
    let mut active_tags = Vec::<String>::new();
    let mut offset = 0;
    let mut output = Vec::new();
    for (index, line) in source.split_inclusive('\n').enumerate() {
        let line_range = range(offset, offset + line.len());
        if checkpoint.is_cancelled() {
            return Err(line_range);
        }
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
            .is_none_or(|lines| lines.contains(number));
        if tag_selected && line_selected {
            output.push(SelectedLine {
                text: line.to_owned(),
                range: line_range,
                mapping: SourceMapping::Identity,
            });
        }
        offset += line.len();
    }
    Ok(output)
}

fn tag_marker<'a>(value: &'a str, marker: &str) -> Option<&'a str> {
    let offset = value.find(marker)?;
    let rest = &value[offset + marker.len()..];
    rest.strip_suffix("[]")
}

#[derive(Debug, Eq, PartialEq)]
struct LineSelection {
    ranges: Vec<(usize, usize)>,
}

impl LineSelection {
    fn contains(&self, line: usize) -> bool {
        let index = self
            .ranges
            .partition_point(|(_, range_end)| *range_end < line);
        self.ranges
            .get(index)
            .is_some_and(|(range_start, range_end)| *range_start <= line && line <= *range_end)
    }
}

fn parse_line_selection(
    value: &str,
    cancellation: &dyn CancellationCheck,
) -> Result<LineSelection, TextRange> {
    let mut checkpoint = CancellationCheckpoint::new(cancellation);
    let mut ranges = BTreeMap::<usize, usize>::new();
    for item in value.split([';', ',']) {
        if checkpoint.is_cancelled() {
            return Err(zero_range());
        }
        if let Some((start, end)) = item.trim().split_once("..") {
            if let (Ok(start), Ok(end)) = (start.parse::<u128>(), end.parse::<u128>())
                && start <= end
                && start <= usize::MAX as u128
            {
                let start = start as usize;
                let end = end.min(usize::MAX as u128) as usize;
                ranges
                    .entry(start)
                    .and_modify(|previous| *previous = (*previous).max(end))
                    .or_insert(end);
            }
        } else if let Ok(line) = item.trim().parse::<u128>()
            && line <= usize::MAX as u128
        {
            let line = line as usize;
            ranges.entry(line).or_insert(line);
        }
    }
    let mut normalized: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        if checkpoint.is_cancelled() {
            return Err(zero_range());
        }
        if let Some((_, previous_end)) = normalized.last_mut()
            && start <= previous_end.saturating_add(1)
        {
            *previous_end = (*previous_end).max(end);
        } else {
            normalized.push((start, end));
        }
    }
    Ok(LineSelection { ranges: normalized })
}

/// Why a selected include body could not be transformed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TransformFailure {
    Cancelled,
    ByteLimit,
}

/// Applies `indent` and `leveloffset` to the selected include body.
///
/// The `indent` attribute comes from the document and grows every selected
/// line, so the padding is charged against the remaining expansion budget
/// before it is allocated. Charging afterwards would let a single directive
/// materialize far more text than `max_total_bytes` permits.
fn transform_lines(
    lines: Vec<SelectedLine>,
    attributes: &BTreeMap<String, String>,
    remaining_bytes: usize,
    cancellation: &dyn CancellationCheck,
) -> Result<Vec<SelectedLine>, TransformFailure> {
    let mut checkpoint = CancellationCheckpoint::new(cancellation);
    let indent = attributes
        .get("indent")
        .and_then(|value| value.parse::<i32>().ok())
        .unwrap_or(0);
    let leveloffset = attributes
        .get("leveloffset")
        .and_then(|value| value.parse::<i32>().ok())
        .unwrap_or(0);
    let mut output = Vec::with_capacity(lines.len());
    let mut charged_padding = 0usize;
    for mut line in lines {
        if checkpoint.is_cancelled() {
            return Err(TransformFailure::Cancelled);
        }
        let original = line.text.clone();
        if leveloffset != 0 {
            line.text = apply_leveloffset(&line.text, leveloffset);
        }
        if indent > 0 {
            let padding = indent.unsigned_abs() as usize;
            charged_padding = charged_padding.saturating_add(padding);
            if charged_padding > remaining_bytes {
                return Err(TransformFailure::ByteLimit);
            }
            let mut padded = String::with_capacity(padding.saturating_add(line.text.len()));
            padded.extend(std::iter::repeat_n(' ', padding));
            padded.push_str(&line.text);
            line.text = padded;
        } else if indent < 0 {
            let remove = indent.unsigned_abs() as usize;
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
        output.push(line);
    }
    Ok(output)
}

fn apply_leveloffset(line: &str, offset: i32) -> String {
    let marker_count = line.bytes().take_while(|byte| *byte == b'=').count();
    if marker_count == 0 || line.as_bytes().get(marker_count) != Some(&b' ') {
        return line.to_owned();
    }
    let adjusted = i32::try_from(marker_count)
        .unwrap_or(i32::MAX)
        .saturating_add(offset)
        .clamp(1, 6) as usize;
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::cancellation::CHECKPOINT_INTERVAL;

    struct CancelAfter {
        checks: AtomicUsize,
        completed_checks: usize,
    }

    impl CancellationCheck for CancelAfter {
        fn is_cancelled(&self) -> bool {
            self.checks.fetch_add(1, Ordering::Relaxed) >= self.completed_checks
        }
    }

    #[test]
    fn preprocessing_cancels_at_a_bounded_line_checkpoint() {
        let cancellation = CancelAfter {
            checks: AtomicUsize::new(0),
            completed_checks: 2,
        };
        let source = "paragraph\n".repeat(CHECKPOINT_INTERVAL * 3);

        let failure = preprocess_cancellable(
            &source,
            &ResourceSnapshot::default(),
            &PreprocessOptions::default(),
            &cancellation,
        )
        .expect_err("preprocessing should be cancelled");

        assert_eq!(failure, PreprocessFailure::Cancelled);
        assert_eq!(cancellation.checks.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn noncancellable_preprocessing_facade_preserves_output() {
        let source = "first\nsecond\n";
        let expected = preprocess(
            source,
            &ResourceSnapshot::default(),
            &PreprocessOptions::default(),
        )
        .expect("preprocess");
        let actual = preprocess_cancellable(
            source,
            &ResourceSnapshot::default(),
            &PreprocessOptions::default(),
            &NeverCancel,
        )
        .expect("cancellable preprocess");

        assert_eq!(actual, expected);
    }

    #[test]
    fn enormous_line_range_is_not_materialized() {
        let mut snapshot = ResourceSnapshot::default();
        snapshot.insert(
            "part.adoc",
            ResourceDocument {
                source_id: SourceId::new("part"),
                source: "first\nsecond\n".into(),
            },
        );

        let document = preprocess(
            "include::part.adoc[lines=1..18446744073709551615]\n",
            &snapshot,
            &PreprocessOptions::default(),
        )
        .expect("bounded line selection");

        assert_eq!(document.source, "first\nsecond\n");
    }

    #[test]
    fn line_selection_parsing_remains_cancellable() {
        struct CancelAfterFirstCheckpoint(AtomicUsize);

        impl CancellationCheck for CancelAfterFirstCheckpoint {
            fn is_cancelled(&self) -> bool {
                self.0.fetch_add(1, Ordering::Relaxed) >= 1
            }
        }

        let value = (0..CHECKPOINT_INTERVAL * 3)
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let cancellation = CancelAfterFirstCheckpoint(AtomicUsize::new(0));

        assert_eq!(
            parse_line_selection(&value, &cancellation),
            Err(zero_range())
        );
        assert_eq!(cancellation.0.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn line_selection_normalizes_unordered_overlapping_and_boundary_ranges() {
        assert_eq!(
            parse_line_selection("5..8,1,2..4,7..10,12,12,11,20..19,not-a-line", &NeverCancel,)
                .expect("line selection"),
            LineSelection {
                ranges: vec![(1, 12)]
            }
        );

        let maximum = usize::MAX as u128;
        assert_eq!(
            parse_line_selection(
                &format!("{}..{},{}", maximum - 1, u128::MAX, maximum),
                &NeverCancel,
            )
            .expect("boundary line selection"),
            LineSelection {
                ranges: vec![(usize::MAX - 1, usize::MAX)]
            }
        );
        assert_eq!(
            parse_line_selection(&(maximum + 1).to_string(), &NeverCancel)
                .expect("out-of-range line selection"),
            LineSelection { ranges: Vec::new() }
        );
    }

    #[test]
    fn combined_processing_classifies_preprocess_cancellation_separately() {
        let cancellation = crate::core::CancellationToken::new();
        cancellation.cancel();

        assert!(matches!(
            preprocess_and_analyze_cancellable(
                &Engine::new(crate::core::AnalysisOptions::default()),
                "paragraph\n",
                &ResourceSnapshot::default(),
                &PreprocessOptions::default(),
                &cancellation,
            ),
            Err(PreprocessedAnalysisError::Cancelled)
        ));
    }

    #[test]
    fn never_cancel_combined_processing_preserves_success_and_preprocess_errors() {
        let mut snapshot = ResourceSnapshot::default();
        snapshot.insert(
            "part.adoc",
            ResourceDocument {
                source_id: SourceId::new("part"),
                source: "included\n".into(),
            },
        );
        let engine = Engine::new(crate::core::AnalysisOptions::default());
        let options = PreprocessOptions::default();
        let expected =
            preprocess_and_analyze(&engine, "include::part.adoc[]\n", &snapshot, &options)
                .expect("compatibility analysis");
        let actual = preprocess_and_analyze_cancellable(
            &engine,
            "include::part.adoc[]\n",
            &snapshot,
            &options,
            &NeverCancel,
        )
        .expect("cancellable analysis");

        assert_eq!(actual.document, expected.document);
        assert_eq!(
            actual.analysis.document().snapshot(),
            expected.analysis.document().snapshot()
        );
        assert_eq!(
            actual.analysis.diagnostics(),
            expected.analysis.diagnostics()
        );

        let expected_error =
            preprocess_and_analyze(&engine, "include::missing.adoc[]\n", &snapshot, &options)
                .expect_err("compatibility preprocessing error");
        let actual_error = preprocess_and_analyze_cancellable(
            &engine,
            "include::missing.adoc[]\n",
            &snapshot,
            &options,
            &NeverCancel,
        )
        .expect_err("cancellable preprocessing error");
        assert_eq!(actual_error, expected_error);
    }

    #[test]
    fn include_conditionals_filters_and_source_map_are_deterministic() {
        let mut snapshot = ResourceSnapshot::default();
        snapshot.insert(
            "part.adoc",
            ResourceDocument {
                source_id: SourceId::new("part"),
                source: "// tag::keep[]\n= Included\nline one\nline two\n// end::keep[]\n".into(),
            },
        );
        let mut options = PreprocessOptions {
            source_id: Some(SourceId::new("root")),
            ..PreprocessOptions::default()
        };
        options
            .attributes
            .insert("enabled".to_owned(), Some("".to_owned()));
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
    fn include_indent_is_charged_before_the_padding_is_allocated() {
        let mut snapshot = ResourceSnapshot::default();
        snapshot.insert(
            "part.adoc",
            ResourceDocument {
                source_id: SourceId::new("part"),
                source: "one\ntwo\nthree\n".into(),
            },
        );
        let options = PreprocessOptions {
            source_id: Some(SourceId::new("root")),
            max_total_bytes: 1024,
            ..PreprocessOptions::default()
        };

        // Charging before the allocation reports the limit against the include
        // directive that requested the padding. Charging afterwards would report
        // it against a line of the included resource, after that line was built.
        let error = preprocess(
            "line\ninclude::part.adoc[indent=4096]\n",
            &snapshot,
            &options,
        )
        .expect_err("indent byte limit");
        assert_eq!(error.kind, PreprocessErrorKind::ByteLimit);
        assert_eq!(error.source_id.as_ref().map(SourceId::as_str), Some("root"));
        assert_eq!(error.range, range(5, 37));

        let document = preprocess(
            "include::part.adoc[indent=2]\n",
            &snapshot,
            &PreprocessOptions {
                source_id: Some(SourceId::new("root")),
                ..PreprocessOptions::default()
            },
        )
        .expect("indent within the budget");
        assert_eq!(document.source, "  one\n  two\n  three\n");
    }

    #[test]
    fn include_indent_and_leveloffset_extremes_do_not_overflow() {
        let mut snapshot = ResourceSnapshot::default();
        snapshot.insert(
            "part.adoc",
            ResourceDocument {
                source_id: SourceId::new("part"),
                source: "    = Included\n    body\n".into(),
            },
        );

        let dedented = preprocess(
            "include::part.adoc[indent=-2147483648]\n",
            &snapshot,
            &PreprocessOptions::default(),
        )
        .expect("minimum indent");
        assert_eq!(dedented.source, "= Included\nbody\n");

        let raised = preprocess(
            "include::part.adoc[leveloffset=2147483647]\n",
            &snapshot,
            &PreprocessOptions::default(),
        )
        .expect("maximum leveloffset");
        assert_eq!(raised.source, "    = Included\n    body\n");

        let lowered = preprocess(
            "include::part.adoc[indent=-4,leveloffset=-2147483648]\n",
            &snapshot,
            &PreprocessOptions::default(),
        )
        .expect("minimum leveloffset");
        assert_eq!(lowered.source, "= Included\nbody\n");
    }

    #[test]
    fn include_attributes_are_quote_aware_and_optional_missing_resources_are_notices() {
        let mut snapshot = ResourceSnapshot::default();
        snapshot.insert(
            "part.adoc",
            ResourceDocument {
                source_id: SourceId::new("part"),
                source: "// tag::one[]\none\n// end::one[]\n// tag::two[]\ntwo\n// end::two[]\n"
                    .into(),
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
                source: "include::cycle.adoc[]\n".into(),
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
            .insert("edition".to_owned(), Some("2".to_owned()));
        options
            .attributes
            .insert("web".to_owned(), Some(String::new()));
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
                    source: source.into(),
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
                    source: format!("{target}\n").into(),
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
            attributes: BTreeMap::from([("locked".to_owned(), Some("host".to_owned()))]),
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

        let mut analysis_options = crate::AnalysisOptions::default();
        analysis_options.attributes.clone_from(&options.attributes);
        let analyzed =
            preprocess_and_analyze(&Engine::new(analysis_options), source, &snapshot, &options)
                .expect("preprocessed analysis");
        let locked = analyzed
            .analysis
            .attribute_environment()
            .resolve_at(
                "locked",
                TextSize::new(analyzed.document.source.len()).expect("offset"),
            )
            .expect("locked attribute");
        assert_eq!(locked.value, Ok(Some("host")));
        assert_eq!(locked.binding, None);
    }

    #[test]
    fn base_uri_resolves_snapshot_keys_without_io() {
        let mut snapshot = ResourceSnapshot::default();
        snapshot.insert(
            "chapters/one.adoc",
            ResourceDocument {
                source_id: SourceId::new("one"),
                source: "chapter\n".into(),
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
                source: "include::section.adoc[]\n".into(),
            },
        );
        snapshot.insert(
            "book/chapters/section.adoc",
            ResourceDocument {
                source_id: SourceId::new("section"),
                source: "nested\n".into(),
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
                    .into(),
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
                source: included.into(),
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
                source: included.into(),
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
                source: included.into(),
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
                source: included.into(),
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
