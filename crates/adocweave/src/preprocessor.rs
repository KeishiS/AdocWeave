//! Pure preprocessing over caller-provided resource snapshots.

mod directive;
mod expansion;
mod projection;
mod source_map;

pub(crate) use directive::{DirectiveLine, classify_line};

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

#[cfg(test)]
thread_local! {
    static RESUMABLE_INCLUDE_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static RESUMABLE_LINE_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

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

/// Result of consulting a host-owned resource collection.
///
/// `Deferred` distinguishes a resource that a host may still acquire from a
/// resource whose absence is authoritative for this preprocessing run.
/// This enum is non-exhaustive so new host outcomes can be added without a
/// breaking API change. Callers must retain a fallback match arm.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ResourceLookupResult {
    /// The resource is ready for deterministic preprocessing.
    Ready(ResourceDocument),
    /// The host has established that the resource does not exist.
    Missing,
    /// The host must acquire or otherwise resolve the resource before work can continue.
    Deferred,
    /// The host could not load the resource and preprocessing cannot continue.
    Failed(String),
}

/// Read-only resource boundary used by resumable preprocessing.
pub trait ResourceLookup {
    /// Looks up one validated, resolved snapshot key.
    ///
    /// The lookup view must remain stable for the lifetime of one preprocessing
    /// run. A host must not replace its workspace generation in place: it
    /// starts a new run with a new view instead. Answers already observed by
    /// the machine, including answers supplied after `Deferred`, are retained
    /// and reused for the remainder of the run.
    fn lookup(&self, target: &str) -> ResourceLookupResult;
}

impl ResourceLookup for ResourceSnapshot {
    fn lookup(&self, target: &str) -> ResourceLookupResult {
        self.get(target)
            .cloned()
            .map_or(ResourceLookupResult::Missing, ResourceLookupResult::Ready)
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

/// Optional inputs for one preprocessing run.
///
/// Every field defaults to absent, so callers name only what they need:
/// `PreprocessInputs { cancellation: Some(&token) }`.
#[derive(Default)]
pub struct PreprocessInputs<'inputs> {
    /// Cooperative cancellation checked at bounded checkpoints.
    pub cancellation: Option<&'inputs dyn CancellationCheck>,
}

impl PreprocessInputs<'_> {
    fn cancellation(&self) -> &dyn CancellationCheck {
        self.cancellation.unwrap_or(&NeverCancel)
    }
}

/// Expands a caller-provided snapshot and analyzes the resulting text.
pub fn preprocess_and_analyze(
    engine: &Engine,
    source: &str,
    snapshot: &ResourceSnapshot,
    options: &PreprocessOptions,
) -> Result<PreprocessedAnalysis, PreprocessedAnalysisError> {
    preprocess_and_analyze_with(
        engine,
        source,
        snapshot,
        options,
        PreprocessInputs::default(),
    )
}

/// Expands and analyzes caller-provided input with optional inputs.
pub fn preprocess_and_analyze_with(
    engine: &Engine,
    source: &str,
    snapshot: &ResourceSnapshot,
    options: &PreprocessOptions,
    inputs: PreprocessInputs<'_>,
) -> Result<PreprocessedAnalysis, PreprocessedAnalysisError> {
    let options = EffectiveProcessingOptions::new(engine.options().clone(), options.clone())
        .map_err(PreprocessedAnalysisError::Options)?;
    options.preprocess_and_analyze(source, snapshot, inputs)
}

impl EffectiveProcessingOptions {
    /// Expands and analyzes with this already validated configuration.
    ///
    /// Callers that validate once and process many documents use this instead
    /// of the free functions, which validate on every call.
    pub fn preprocess_and_analyze(
        &self,
        source: &str,
        snapshot: &ResourceSnapshot,
        inputs: PreprocessInputs<'_>,
    ) -> Result<PreprocessedAnalysis, PreprocessedAnalysisError> {
        preprocess_and_analyze_effective(source, snapshot, self, inputs.cancellation())
    }
}

fn preprocess_and_analyze_effective(
    source: &str,
    snapshot: &ResourceSnapshot,
    options: &EffectiveProcessingOptions,
    cancellation: &dyn CancellationCheck,
) -> Result<PreprocessedAnalysis, PreprocessedAnalysisError> {
    let document = preprocess_with(
        source,
        snapshot,
        options.preprocess(),
        PreprocessInputs {
            cancellation: Some(cancellation),
        },
    )
    .map_err(|failure| match failure {
        PreprocessFailure::Error(error) => PreprocessedAnalysisError::Preprocess(error),
        PreprocessFailure::Cancelled => PreprocessedAnalysisError::Cancelled,
    })?;
    let analysis = Engine::new(options.analysis().clone())
        .analyze_with(
            &document.source,
            crate::AnalysisInputs {
                source_id: options.preprocess().source_id.as_ref(),
                cancellation: Some(cancellation),
            },
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
    match preprocess_with(source, snapshot, options, PreprocessInputs::default()) {
        Ok(document) => Ok(document),
        Err(PreprocessFailure::Error(error)) => Err(error),
        Err(PreprocessFailure::Cancelled) => {
            unreachable!("NeverCancel cannot cancel preprocessing")
        }
    }
}

/// Expands a caller-provided snapshot with optional inputs.
pub fn preprocess_with(
    source: &str,
    snapshot: &ResourceSnapshot,
    options: &PreprocessOptions,
    inputs: PreprocessInputs<'_>,
) -> Result<PreprocessedDocument, PreprocessFailure> {
    let cancellation = inputs.cancellation();
    match preprocess_resumable(source, options, snapshot, cancellation) {
        PreprocessStep::Complete(document) => Ok(document),
        PreprocessStep::Failed(error) => Err(PreprocessFailure::Error(error)),
        PreprocessStep::HostError(_) => {
            unreachable!("ResourceSnapshot cannot report a host loading failure")
        }
        PreprocessStep::Cancelled => Err(PreprocessFailure::Cancelled),
        PreprocessStep::NeedResource(_) => {
            unreachable!("ResourceSnapshot reports authoritative absence")
        }
    }
}

/// One resource requested by a suspended preprocessing run.
#[derive(Debug)]
struct ResourceCorrelation;

#[derive(Clone, Debug)]
pub struct ResourceRequest {
    target: String,
    optional: bool,
    source_id: Option<SourceId>,
    range: TextRange,
    correlation: Arc<ResourceCorrelation>,
}

impl PartialEq for ResourceRequest {
    fn eq(&self, other: &Self) -> bool {
        self.target == other.target
            && self.optional == other.optional
            && self.source_id == other.source_id
            && self.range == other.range
            && Arc::ptr_eq(&self.correlation, &other.correlation)
    }
}

impl Eq for ResourceRequest {}

impl ResourceRequest {
    /// Returns the resolved snapshot key.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// Returns whether the include declared the resource optional.
    pub const fn is_optional(&self) -> bool {
        self.optional
    }

    /// Returns the source containing the include directive.
    pub fn source_id(&self) -> Option<&SourceId> {
        self.source_id.as_ref()
    }

    /// Returns the source range of the include directive.
    pub const fn range(&self) -> TextRange {
        self.range
    }

    /// Builds the response for this request when loading succeeds.
    pub fn found(&self, document: ResourceDocument) -> ResourceResponse {
        ResourceResponse {
            correlation: Arc::clone(&self.correlation),
            outcome: ResourceResponseOutcome::Found(document),
        }
    }

    /// Builds the response for this request when absence is authoritative.
    pub fn not_found(&self) -> ResourceResponse {
        ResourceResponse {
            correlation: Arc::clone(&self.correlation),
            outcome: ResourceResponseOutcome::NotFound,
        }
    }

    /// Builds a terminal host-load failure for this request.
    pub fn load_failed(&self, message: impl Into<String>) -> ResourceResponse {
        ResourceResponse {
            correlation: Arc::clone(&self.correlation),
            outcome: ResourceResponseOutcome::LoadFailed(message.into()),
        }
    }
}

/// Authoritative answer supplied when suspended preprocessing resumes.
///
/// Responses can only be built from the matching [`ResourceRequest`]. The
/// continuation verifies that correlation before accepting the answer.
#[derive(Clone, Debug)]
pub struct ResourceResponse {
    correlation: Arc<ResourceCorrelation>,
    outcome: ResourceResponseOutcome,
}

impl PartialEq for ResourceResponse {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.correlation, &other.correlation) && self.outcome == other.outcome
    }
}

impl Eq for ResourceResponse {}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ResourceResponseOutcome {
    Found(ResourceDocument),
    NotFound,
    LoadFailed(String),
}

/// A terminal failure at the host-owned resource boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostResourceError {
    kind: HostResourceErrorKind,
    target: String,
    message: String,
}

impl HostResourceError {
    pub const fn kind(&self) -> HostResourceErrorKind {
        self.kind
    }

    pub fn target(&self) -> &str {
        &self.target
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for HostResourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for HostResourceError {}

/// Stable category for a host resource failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum HostResourceErrorKind {
    /// The host failed while loading the requested resource.
    LoadFailed,
    /// A response was built from a different or stale request.
    ResponseMismatch,
}

/// Result of starting or resuming preprocessing.
///
/// This enum is non-exhaustive so future suspension and terminal states can be
/// added compatibly. Callers must retain a fallback match arm.
#[non_exhaustive]
pub enum PreprocessStep {
    /// Preprocessing completed and produced one immutable document.
    Complete(PreprocessedDocument),
    /// Processing stopped before the first resource whose availability is deferred.
    NeedResource(Box<SuspendedPreprocess>),
    /// Processing failed with a deterministic preprocessing error.
    Failed(PreprocessError),
    /// The host failed to satisfy the resource-loading contract.
    HostError(HostResourceError),
    /// Cooperative cancellation discarded all unpublished state.
    Cancelled,
}

/// Opaque, single-use continuation for one deferred resource request.
///
/// The type intentionally does not implement `Clone`: exactly one response can
/// advance the accumulated attributes, limits, include stack, and source map.
pub struct SuspendedPreprocess {
    machine: PreprocessMachine,
    pending: PendingInclude,
    request: ResourceRequest,
}

impl SuspendedPreprocess {
    /// Returns the resource request that must be answered before resuming.
    pub const fn request(&self) -> &ResourceRequest {
        &self.request
    }

    /// Consumes this continuation and resumes from the suspended include.
    pub fn resume(
        mut self,
        response: ResourceResponse,
        resources: &(impl ResourceLookup + ?Sized),
        cancellation: &dyn CancellationCheck,
    ) -> PreprocessStep {
        if cancellation.is_cancelled() {
            return PreprocessStep::Cancelled;
        }
        if !Arc::ptr_eq(&self.request.correlation, &response.correlation) {
            return PreprocessStep::HostError(HostResourceError {
                kind: HostResourceErrorKind::ResponseMismatch,
                target: self.request.target,
                message: "resource response does not match the suspended request".to_owned(),
            });
        }
        let document = match response.outcome {
            ResourceResponseOutcome::Found(document) => Some(document),
            ResourceResponseOutcome::NotFound => None,
            ResourceResponseOutcome::LoadFailed(message) => {
                return PreprocessStep::HostError(HostResourceError {
                    kind: HostResourceErrorKind::LoadFailed,
                    target: self.request.target,
                    message,
                });
            }
        };
        self.machine
            .resolved
            .insert(self.request.target.clone(), document.clone());
        let child = match self
            .machine
            .resolve_pending(self.pending, document, cancellation)
        {
            Ok(child) => child,
            Err(failure) => return failure.into_step(),
        };
        if let Some(child) = child {
            self.machine.push_cursor(child);
        }
        self.machine.drive(resources, cancellation)
    }
}

/// Starts preprocessing that may suspend when the lookup returns `Deferred`.
pub fn preprocess_resumable(
    source: &str,
    options: &PreprocessOptions,
    resources: &(impl ResourceLookup + ?Sized),
    cancellation: &dyn CancellationCheck,
) -> PreprocessStep {
    if cancellation.is_cancelled() {
        return PreprocessStep::Cancelled;
    }
    let mut machine = PreprocessMachine {
        options: options.clone(),
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
        stack: Vec::new(),
        resolved: BTreeMap::new(),
        until_cancel_check: 0,
    };
    let lines = match machine.lines(source, cancellation) {
        Ok(lines) => lines,
        Err(failure) => return failure.into_step(),
    };
    let root = IncludeFrame::root(options.source_id.clone(), options.base_uri.as_deref());
    let frame = match ExpansionCursor::new(lines, root) {
        Ok(frame) => frame,
        Err(error) => return PreprocessStep::Failed(error),
    };
    machine.push_cursor(frame);
    machine.drive(resources, cancellation)
}

struct PreprocessMachine {
    options: PreprocessOptions,
    source_map: source_map::SourceMapBuilder,
    directives: Vec<Directive>,
    notices: Vec<PreprocessNotice>,
    state: ExpansionState,
    stack: Vec<ExpansionCursor>,
    resolved: BTreeMap<String, Option<ResourceDocument>>,
    until_cancel_check: usize,
}

struct ExpansionCursor {
    lines: Vec<SelectedLine>,
    document: crate::source_document::SourceDocument,
    frame: IncludeFrame,
    next_line: usize,
    conditions: Vec<bool>,
    attribute_value_through: Option<usize>,
}

impl ExpansionCursor {
    fn new(lines: Vec<SelectedLine>, frame: IncludeFrame) -> Result<Self, PreprocessError> {
        let source_id = frame.source_id();
        let selected_source = lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<String>();
        let document =
            crate::source_document::SourceDocument::new(&selected_source).map_err(|_| {
                error(
                    PreprocessErrorKind::InternalInvariant,
                    source_id.clone(),
                    zero_range(),
                    "selected source exceeds the supported position range",
                )
            })?;
        if document.lines().len() < lines.len() {
            return Err(error(
                PreprocessErrorKind::InternalInvariant,
                source_id,
                zero_range(),
                "selected source lines do not preserve physical boundaries",
            ));
        }
        Ok(Self {
            lines,
            document,
            frame,
            next_line: 0,
            conditions: Vec::new(),
            attribute_value_through: None,
        })
    }
}

struct PendingInclude {
    frame: IncludeFrame,
    source_id: Option<SourceId>,
    range: TextRange,
    target_range: TextRange,
    expanded_target: String,
    target: String,
    attributes: BTreeMap<String, String>,
    optional: bool,
}

enum MachineFailure {
    Error(PreprocessError),
    Cancelled,
}

enum MachineLookup {
    Resolved(Option<ResourceDocument>),
    Deferred(ResourceRequest),
    Failed(HostResourceError),
}

impl MachineFailure {
    fn into_step(self) -> PreprocessStep {
        match self {
            Self::Error(error) => PreprocessStep::Failed(error),
            Self::Cancelled => PreprocessStep::Cancelled,
        }
    }
}

impl From<PreprocessError> for MachineFailure {
    fn from(error: PreprocessError) -> Self {
        Self::Error(error)
    }
}

impl PreprocessMachine {
    fn lines(
        &mut self,
        source: &str,
        cancellation: &dyn CancellationCheck,
    ) -> Result<Vec<SelectedLine>, MachineFailure> {
        let mut offset = 0;
        let mut lines = Vec::new();
        for line in source.split_inclusive('\n') {
            let start = offset;
            offset += line.len();
            let line_range = range(start, offset);
            self.check_cancelled(cancellation)?;
            lines.push(SelectedLine {
                text: line.to_owned(),
                range: line_range,
                mapping: SourceMapping::Identity,
            });
        }
        Ok(lines)
    }

    fn prepare_include(
        &mut self,
        include: ParsedDirective,
        frame: &IncludeFrame,
        range: TextRange,
    ) -> Result<PendingInclude, MachineFailure> {
        #[cfg(test)]
        RESUMABLE_INCLUDE_VISITS.with(|visits| visits.set(visits.get().saturating_add(1)));
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
        let expanded_target = directive::expand_attributes(
            &include.target,
            self.state.attributes(),
            self.state.attribute_limits(),
        );
        let target = resolve_include_target(&expanded_target, frame.base_uri());
        validate_target(&target, &self.options).map_err(|message| {
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
        Ok(PendingInclude {
            frame: frame.clone(),
            source_id,
            range,
            target_range: relative_range(range, include.target_start, include.target_end),
            expanded_target,
            target,
            attributes,
            optional,
        })
    }

    fn resolve_pending(
        &mut self,
        pending: PendingInclude,
        document: Option<ResourceDocument>,
        cancellation: &dyn CancellationCheck,
    ) -> Result<Option<ExpansionCursor>, MachineFailure> {
        let PendingInclude {
            frame,
            source_id,
            range,
            target_range,
            expanded_target,
            target,
            attributes,
            optional,
        } = pending;
        self.directives.push(Directive {
            kind: DirectiveKind::Include,
            source_id: source_id.clone(),
            range,
            authored_target: Some(expanded_target.clone()),
            optional,
            target: target.clone(),
            target_range,
            resource_source_id: document.as_ref().map(|document| document.source_id.clone()),
        });
        let Some(document) = document else {
            if optional {
                self.notices.push(PreprocessNotice {
                    kind: PreprocessNoticeKind::OptionalResourceMissing,
                    source_id,
                    range,
                    target,
                });
                return Ok(None);
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
        let selected = select_lines(&document.source, &attributes, cancellation)
            .map_err(|_| MachineFailure::Cancelled)?;
        let remaining_bytes = self.source_map.remaining_bytes();
        let transformed = transform_lines(selected, &attributes, remaining_bytes, cancellation)
            .map_err(|failure| match failure {
                TransformFailure::Cancelled => MachineFailure::Cancelled,
                TransformFailure::ByteLimit => error(
                    PreprocessErrorKind::ByteLimit,
                    source_id.clone(),
                    range,
                    "preprocessor byte limit exceeded",
                )
                .into(),
            })?;
        let child = frame.child(
            target.clone(),
            document.source_id.clone(),
            target_base(&target),
        );
        Ok(Some(ExpansionCursor::new(transformed, child)?))
    }

    fn drive(
        mut self,
        resources: &(impl ResourceLookup + ?Sized),
        cancellation: &dyn CancellationCheck,
    ) -> PreprocessStep {
        loop {
            let Some(mut cursor) = self.stack.pop() else {
                return self.finish(cancellation);
            };
            if cursor.next_line >= cursor.lines.len() {
                if !cursor.conditions.is_empty() {
                    return PreprocessStep::Failed(error(
                        PreprocessErrorKind::UnclosedConditional,
                        cursor.frame.source_id(),
                        zero_range(),
                        "conditional directive is not closed",
                    ));
                }
                continue;
            }
            if self.check_cancelled(cancellation).is_err() {
                return PreprocessStep::Cancelled;
            }
            let line_index = cursor.next_line;
            cursor.next_line += 1;
            #[cfg(test)]
            RESUMABLE_LINE_VISITS.with(|visits| visits.set(visits.get().saturating_add(1)));
            let line = cursor.lines[line_index].clone();
            let source_id = cursor.frame.source_id();
            let content = line.text.trim_end_matches(['\r', '\n']);
            let enabled = cursor.conditions.iter().all(|condition| *condition);
            if cursor
                .attribute_value_through
                .is_some_and(|last_line| line_index <= last_line)
            {
                if let Err(failure) = self.bump_node(source_id.clone(), line.range) {
                    return failure.into_step();
                }
                if let Err(failure) =
                    self.append(&line.text, source_id.clone(), line.range, line.mapping)
                {
                    return failure.into_step();
                }
                if cursor.attribute_value_through == Some(line_index) {
                    cursor.attribute_value_through = None;
                }
                self.push_cursor(cursor);
                continue;
            }
            match directive::recognize(content) {
                RecognizedDirective::Conditional(directive) => {
                    if let Err(failure) = self.process_conditional(
                        &mut cursor,
                        directive,
                        &line,
                        content.len(),
                        enabled,
                    ) {
                        return failure.into_step();
                    }
                    self.push_cursor(cursor);
                }
                RecognizedDirective::Include(include) if enabled => {
                    if self.options.enable_includes {
                        let pending = match self.prepare_include(include, &cursor.frame, line.range)
                        {
                            Ok(pending) => pending,
                            Err(failure) => return failure.into_step(),
                        };
                        self.push_cursor(cursor);
                        match self.lookup_resource(&pending, resources) {
                            MachineLookup::Resolved(document) => {
                                match self.resolve_pending(pending, document, cancellation) {
                                    Ok(Some(child)) => self.push_cursor(child),
                                    Ok(None) => {}
                                    Err(failure) => return failure.into_step(),
                                }
                            }
                            MachineLookup::Deferred(request) => {
                                return PreprocessStep::NeedResource(Box::new(
                                    SuspendedPreprocess {
                                        machine: self,
                                        pending,
                                        request,
                                    },
                                ));
                            }
                            MachineLookup::Failed(error) => {
                                return PreprocessStep::HostError(error);
                            }
                        }
                    } else {
                        if let Err(failure) =
                            self.process_unexpanded_include(include, &line, source_id)
                        {
                            return failure.into_step();
                        }
                        self.push_cursor(cursor);
                    }
                }
                RecognizedDirective::Escaped(literal) if enabled => {
                    if let Err(failure) =
                        self.process_escaped(literal, &line, content.len(), source_id)
                    {
                        return failure.into_step();
                    }
                    self.push_cursor(cursor);
                }
                RecognizedDirective::Text if enabled => {
                    if let Err(failure) =
                        self.process_text(&mut cursor, line_index, &line, content, cancellation)
                    {
                        return failure.into_step();
                    }
                    self.push_cursor(cursor);
                }
                RecognizedDirective::Include(_)
                | RecognizedDirective::Escaped(_)
                | RecognizedDirective::Text => self.push_cursor(cursor),
            }
        }
    }

    fn push_cursor(&mut self, cursor: ExpansionCursor) {
        self.stack.push(cursor);
    }

    fn lookup_resource(
        &mut self,
        pending: &PendingInclude,
        resources: &(impl ResourceLookup + ?Sized),
    ) -> MachineLookup {
        if let Some(cached) = self.resolved.get(&pending.target) {
            return MachineLookup::Resolved(cached.clone());
        }
        let result = resources.lookup(&pending.target);
        match result {
            ResourceLookupResult::Ready(document) => {
                self.resolved
                    .insert(pending.target.clone(), Some(document.clone()));
                MachineLookup::Resolved(Some(document))
            }
            ResourceLookupResult::Missing => {
                self.resolved.insert(pending.target.clone(), None);
                MachineLookup::Resolved(None)
            }
            ResourceLookupResult::Deferred => MachineLookup::Deferred(ResourceRequest {
                target: pending.target.clone(),
                optional: pending.optional,
                source_id: pending.source_id.clone(),
                range: pending.range,
                correlation: Arc::new(ResourceCorrelation),
            }),
            ResourceLookupResult::Failed(message) => MachineLookup::Failed(HostResourceError {
                kind: HostResourceErrorKind::LoadFailed,
                target: pending.target.clone(),
                message,
            }),
        }
    }

    fn process_conditional(
        &mut self,
        cursor: &mut ExpansionCursor,
        directive: ParsedDirective,
        line: &SelectedLine,
        content_len: usize,
        enabled: bool,
    ) -> Result<(), MachineFailure> {
        let source_id = cursor.frame.source_id();
        self.bump_node(source_id.clone(), line.range)?;
        self.directives.push(Directive {
            kind: directive.kind,
            source_id: source_id.clone(),
            range: line.range,
            authored_target: None,
            optional: false,
            target: directive.target.clone(),
            target_range: relative_range(line.range, directive.target_start, directive.target_end),
            resource_source_id: None,
        });
        match directive::transition(
            &directive,
            enabled,
            self.state.attributes(),
            self.state.attribute_limits(),
        ) {
            ConditionalTransition::Inline { selected } => {
                if selected {
                    let ending = &line.text[content_len..];
                    self.append(
                        &format!("{}{ending}", directive.attributes),
                        source_id,
                        line.range,
                        SourceMapping::WholeOrigin,
                    )?;
                    self.state.finish_directive_output();
                }
            }
            ConditionalTransition::Open { enabled } => cursor.conditions.push(enabled),
            ConditionalTransition::Close => {
                if cursor.conditions.pop().is_none() {
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
        Ok(())
    }

    fn process_unexpanded_include(
        &mut self,
        include: ParsedDirective,
        line: &SelectedLine,
        source_id: Option<SourceId>,
    ) -> Result<(), MachineFailure> {
        self.bump_node(source_id.clone(), line.range)?;
        let authored_target = directive::expand_attributes(
            &include.target,
            self.state.attributes(),
            self.state.attribute_limits(),
        );
        let optional = parse_attributes(&include.attributes)
            .is_ok_and(|attributes| attributes.contains_key("optional"));
        self.directives.push(Directive {
            kind: DirectiveKind::Include,
            source_id: source_id.clone(),
            range: line.range,
            authored_target: Some(authored_target),
            optional,
            target: include.target,
            target_range: relative_range(line.range, include.target_start, include.target_end),
            resource_source_id: None,
        });
        self.append(&line.text, source_id, line.range, line.mapping)?;
        self.state.finish_directive_output();
        Ok(())
    }

    fn process_escaped(
        &mut self,
        literal: &str,
        line: &SelectedLine,
        content_len: usize,
        source_id: Option<SourceId>,
    ) -> Result<(), MachineFailure> {
        let ending = &line.text[content_len..];
        self.append(
            &format!("{literal}{ending}"),
            source_id,
            line.range,
            SourceMapping::WholeOrigin,
        )?;
        self.state.finish_directive_output();
        Ok(())
    }

    fn process_text(
        &mut self,
        cursor: &mut ExpansionCursor,
        line_index: usize,
        line: &SelectedLine,
        content: &str,
        cancellation: &dyn CancellationCheck,
    ) -> Result<(), MachineFailure> {
        let source_id = cursor.frame.source_id();
        let delimiter = self.state.observe_delimiter(content);
        let mut document_attribute = false;
        self.bump_node(source_id.clone(), line.range)?;
        if self.state.accepts_attribute(delimiter)
            && crate::attributes::parse_line(
                content,
                cursor.document.lines()[line_index]
                    .content_range()
                    .start()
                    .to_usize(),
                cursor.document.lines()[line_index].full_range(),
            )
            .is_some()
        {
            let parsed = crate::attributes::parse_lines(&cursor.document, line_index, &|| {
                cancellation.is_cancelled()
            })
            .map_err(|failure| match failure {
                crate::parser_support::ParseFailure::Cancelled => MachineFailure::Cancelled,
                crate::parser_support::ParseFailure::Position(_)
                | crate::parser_support::ParseFailure::Budget(_)
                | crate::parser_support::ParseFailure::InternalInvariant => error(
                    PreprocessErrorKind::InternalInvariant,
                    source_id.clone(),
                    line.range,
                    "attribute preprocessing failed",
                )
                .into(),
            })?;
            if let Some((occurrence, _, last_line)) = parsed {
                self.state.apply_attribute(&occurrence);
                document_attribute = true;
                if last_line > line_index {
                    cursor.attribute_value_through = Some(last_line);
                }
            }
        }
        self.append(&line.text, source_id, line.range, line.mapping)?;
        self.state.finish_line(document_attribute, content);
        Ok(())
    }

    fn check_cancelled(
        &mut self,
        cancellation: &dyn CancellationCheck,
    ) -> Result<(), MachineFailure> {
        if self.until_cancel_check == 0 {
            self.until_cancel_check = crate::cancellation::CHECKPOINT_INTERVAL - 1;
            if cancellation.is_cancelled() {
                return Err(MachineFailure::Cancelled);
            }
        } else {
            self.until_cancel_check -= 1;
        }
        Ok(())
    }

    fn bump_node(
        &mut self,
        source_id: Option<SourceId>,
        range: TextRange,
    ) -> Result<(), MachineFailure> {
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
    ) -> Result<(), MachineFailure> {
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
            .map_err(MachineFailure::from)
    }

    fn finish(mut self, cancellation: &dyn CancellationCheck) -> PreprocessStep {
        if cancellation.is_cancelled() {
            return PreprocessStep::Cancelled;
        }
        let mut checkpoint = CancellationCheckpoint::new(cancellation);
        match self.source_map.finish_cancellable(
            std::mem::take(&mut self.directives),
            std::mem::take(&mut self.notices),
            &mut checkpoint,
        ) {
            Ok(_) if cancellation.is_cancelled() => PreprocessStep::Cancelled,
            Ok(document) => PreprocessStep::Complete(document),
            Err(source_map::SourceMapFinishError::Cancelled) => PreprocessStep::Cancelled,
            Err(source_map::SourceMapFinishError::Invariant) => {
                PreprocessStep::Failed(PreprocessError {
                    kind: PreprocessErrorKind::InternalInvariant,
                    source_id: self.options.source_id.clone(),
                    range: zero_range(),
                    requested_target: None,
                    target: None,
                    message:
                        "source map segments are unsorted, overlapping, or outside expanded source"
                            .to_owned(),
                })
            }
        }
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
    use std::cell::Cell;
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

    struct DeferredLookup<'a> {
        snapshot: &'a ResourceSnapshot,
        lookups: Cell<usize>,
    }

    impl ResourceLookup for DeferredLookup<'_> {
        fn lookup(&self, _target: &str) -> ResourceLookupResult {
            self.lookups.set(self.lookups.get().saturating_add(1));
            ResourceLookupResult::Deferred
        }
    }

    fn deferred_preprocess(
        source: &str,
        snapshot: &ResourceSnapshot,
        options: &PreprocessOptions,
    ) -> (
        Result<PreprocessedDocument, PreprocessError>,
        Vec<String>,
        usize,
    ) {
        let lookup = DeferredLookup {
            snapshot,
            lookups: Cell::new(0),
        };
        let mut requests = Vec::new();
        let mut step = preprocess_resumable(source, options, &lookup, &NeverCancel);
        loop {
            match step {
                PreprocessStep::Complete(document) => {
                    return (Ok(document), requests, lookup.lookups.get());
                }
                PreprocessStep::NeedResource(suspended) => {
                    let target = suspended.request().target().to_owned();
                    requests.push(target.clone());
                    let response = lookup.snapshot.get(&target).cloned().map_or_else(
                        || suspended.request().not_found(),
                        |document| suspended.request().found(document),
                    );
                    step = suspended.resume(response, &lookup, &NeverCancel);
                }
                PreprocessStep::Failed(error) => {
                    return (Err(error), requests, lookup.lookups.get());
                }
                PreprocessStep::HostError(error) => panic!("unexpected host error: {error}"),
                PreprocessStep::Cancelled => panic!("NeverCancel cannot cancel preprocessing"),
            }
        }
    }

    fn resource(source_id: &str, source: impl Into<Arc<str>>) -> ResourceDocument {
        ResourceDocument {
            source_id: SourceId::new(source_id),
            source: source.into(),
        }
    }

    #[test]
    fn flat_deferred_resources_resume_once_without_reprocessing_directives() {
        const INCLUDE_COUNT: usize = 32;
        let source = (0..INCLUDE_COUNT)
            .map(|index| format!("include::part-{index}.adoc[]\n"))
            .collect::<String>();
        let snapshot = (0..INCLUDE_COUNT)
            .map(|index| {
                (
                    format!("part-{index}.adoc"),
                    resource(&format!("part-{index}"), format!("part {index}\n")),
                )
            })
            .collect::<ResourceSnapshot>();
        let options = PreprocessOptions::default();
        let expected = preprocess(&source, &snapshot, &options).expect("one-shot preprocessing");
        RESUMABLE_INCLUDE_VISITS.with(|visits| visits.set(0));
        RESUMABLE_LINE_VISITS.with(|visits| visits.set(0));

        let (actual, requests, lookups) = deferred_preprocess(&source, &snapshot, &options);
        let actual = actual.expect("resumable preprocessing");

        assert_eq!(actual, expected);
        assert_eq!(actual.source_map(), expected.source_map());
        assert_eq!(actual.directives, expected.directives);
        assert_eq!(actual.notices, expected.notices);
        assert_eq!(requests.len(), INCLUDE_COUNT);
        assert_eq!(lookups, INCLUDE_COUNT);
        RESUMABLE_INCLUDE_VISITS.with(|visits| assert_eq!(visits.get(), INCLUDE_COUNT));
        RESUMABLE_LINE_VISITS.with(|visits| assert_eq!(visits.get(), INCLUDE_COUNT * 2));
    }

    #[test]
    fn nested_and_attribute_dependent_includes_preserve_read_order() {
        let mut snapshot = ResourceSnapshot::default();
        snapshot.insert(
            "attributes.adoc",
            resource("attributes", ":selected: nested\ninclude::child.adoc[]\n"),
        );
        snapshot.insert(
            "child.adoc",
            resource("child", "child\ninclude::grandchild.adoc[]\n"),
        );
        snapshot.insert("grandchild.adoc", resource("grandchild", "grandchild\n"));
        snapshot.insert("nested.adoc", resource("nested", "selected\n"));
        let source = "include::attributes.adoc[]\ninclude::{selected}.adoc[]\n";
        let options = PreprocessOptions::default();
        let expected = preprocess(source, &snapshot, &options).expect("one-shot preprocessing");
        RESUMABLE_INCLUDE_VISITS.with(|visits| visits.set(0));
        RESUMABLE_LINE_VISITS.with(|visits| visits.set(0));

        let (actual, requests, _) = deferred_preprocess(source, &snapshot, &options);

        assert_eq!(actual.expect("resumable preprocessing"), expected);
        assert_eq!(
            requests,
            [
                "attributes.adoc",
                "child.adoc",
                "grandchild.adoc",
                "nested.adoc"
            ]
        );
        RESUMABLE_INCLUDE_VISITS.with(|visits| assert_eq!(visits.get(), 4));
        RESUMABLE_LINE_VISITS.with(|visits| assert_eq!(visits.get(), 8));
    }

    #[test]
    fn deferred_selection_and_transformations_preserve_unicode_crlf_source_maps() {
        let source =
            "前\r\ninclude::part.adoc[tags=keep,lines=2..4,indent=2,leveloffset=+1]\r\n後\r\n";
        let mut snapshot = ResourceSnapshot::default();
        snapshot.insert(
            "part.adoc",
            resource(
                "part",
                "// tag::keep[]\r\n= 日本語🙂\r\n本文\r\n// end::keep[]\r\n除外\r\n",
            ),
        );
        let expected = preprocess(source, &snapshot, &PreprocessOptions::default())
            .expect("one-shot preprocessing");

        let (actual, requests, _) =
            deferred_preprocess(source, &snapshot, &PreprocessOptions::default());
        let actual = actual.expect("resumable preprocessing");

        assert_eq!(actual, expected);
        assert_eq!(actual.source, "前\r\n  == 日本語🙂\r\n  本文\r\n後\r\n");
        assert_eq!(requests, ["part.adoc"]);
        assert!(
            actual
                .source_map()
                .iter()
                .any(|segment| segment.mapping == SourceMapping::WholeOrigin)
        );
    }

    #[test]
    fn attributes_and_depth_state_survive_multiple_resumes() {
        let source = ":part: child- \\\n  one\ninclude::{part}.adoc[]\n";
        let mut snapshot = ResourceSnapshot::default();
        snapshot.insert(
            "child- one.adoc",
            resource("child", ":next: grandchild\ninclude::{next}.adoc[]\n"),
        );
        snapshot.insert("grandchild.adoc", resource("grandchild", "完了\n"));
        let options = PreprocessOptions {
            max_include_depth: 2,
            ..PreprocessOptions::default()
        };
        let expected = preprocess(source, &snapshot, &options).expect("one-shot preprocessing");

        let (actual, requests, _) = deferred_preprocess(source, &snapshot, &options);

        assert_eq!(actual.expect("resumable preprocessing"), expected);
        assert_eq!(requests, ["child- one.adoc", "grandchild.adoc"]);
    }

    #[test]
    fn terminal_include_validation_precedes_lookup() {
        let snapshot = ResourceSnapshot::default();
        let lookup = DeferredLookup {
            snapshot: &snapshot,
            lookups: Cell::new(0),
        };
        let step = preprocess_resumable(
            "include::../outside.adoc[]\n",
            &PreprocessOptions::default(),
            &lookup,
            &NeverCancel,
        );

        assert!(matches!(
            step,
            PreprocessStep::Failed(PreprocessError {
                kind: PreprocessErrorKind::UnsafeTarget,
                ..
            })
        ));
        assert_eq!(lookup.lookups.get(), 0);
    }

    #[test]
    fn stale_or_wrong_response_is_a_terminal_host_error() {
        let snapshot = ResourceSnapshot::default();
        let lookup = DeferredLookup {
            snapshot: &snapshot,
            lookups: Cell::new(0),
        };
        let PreprocessStep::NeedResource(first) = preprocess_resumable(
            "include::one.adoc[optional]\ninclude::two.adoc[optional]\n",
            &PreprocessOptions::default(),
            &lookup,
            &NeverCancel,
        ) else {
            panic!("first request");
        };
        let stale = first.request().not_found();
        let PreprocessStep::NeedResource(second) =
            first.resume(stale.clone(), &lookup, &NeverCancel)
        else {
            panic!("second request");
        };

        let PreprocessStep::HostError(error) = second.resume(stale, &lookup, &NeverCancel) else {
            panic!("mismatched response must fail");
        };
        assert_eq!(error.kind(), HostResourceErrorKind::ResponseMismatch);
        assert_eq!(error.target(), "two.adoc");
    }

    #[test]
    fn host_load_failure_discards_the_continuation() {
        let snapshot = ResourceSnapshot::default();
        let lookup = DeferredLookup {
            snapshot: &snapshot,
            lookups: Cell::new(0),
        };
        let PreprocessStep::NeedResource(suspended) = preprocess_resumable(
            "include::part.adoc[]\n",
            &PreprocessOptions::default(),
            &lookup,
            &NeverCancel,
        ) else {
            panic!("request");
        };
        let response = suspended.request().load_failed("host read failed");

        let PreprocessStep::HostError(error) = suspended.resume(response, &lookup, &NeverCancel)
        else {
            panic!("load failure must be terminal");
        };
        assert_eq!(error.kind(), HostResourceErrorKind::LoadFailed);
        assert_eq!(error.target(), "part.adoc");
        assert_eq!(error.message(), "host read failed");
    }

    #[test]
    fn synchronous_lookup_failure_is_a_terminal_host_error() {
        struct FailedLookup;

        impl ResourceLookup for FailedLookup {
            fn lookup(&self, _target: &str) -> ResourceLookupResult {
                ResourceLookupResult::Failed("host lookup failed".to_owned())
            }
        }

        let PreprocessStep::HostError(error) = preprocess_resumable(
            "include::part.adoc[]\n",
            &PreprocessOptions::default(),
            &FailedLookup,
            &NeverCancel,
        ) else {
            panic!("lookup failure must be terminal");
        };
        assert_eq!(error.kind(), HostResourceErrorKind::LoadFailed);
        assert_eq!(error.target(), "part.adoc");
        assert_eq!(error.message(), "host lookup failed");
    }

    #[test]
    fn selection_transform_and_resume_stages_observe_cancellation() {
        let finish_cancellation = CancelAfter {
            checks: AtomicUsize::new(0),
            completed_checks: 1,
        };
        assert!(matches!(
            preprocess_resumable(
                "",
                &PreprocessOptions::default(),
                &ResourceSnapshot::default(),
                &finish_cancellation,
            ),
            PreprocessStep::Cancelled
        ));
        assert_eq!(finish_cancellation.checks.load(Ordering::Relaxed), 2);

        let cancellation = crate::core::CancellationToken::new();
        cancellation.cancel();
        assert!(select_lines("one\ntwo\n", &BTreeMap::new(), &cancellation).is_err());
        assert!(matches!(
            transform_lines(
                vec![SelectedLine {
                    text: "one\n".to_owned(),
                    range: range(0, 4),
                    mapping: SourceMapping::Identity,
                }],
                &BTreeMap::new(),
                usize::MAX,
                &cancellation,
            ),
            Err(TransformFailure::Cancelled)
        ));

        let snapshot = ResourceSnapshot::default();
        let lookup = DeferredLookup {
            snapshot: &snapshot,
            lookups: Cell::new(0),
        };
        let PreprocessStep::NeedResource(suspended) = preprocess_resumable(
            "include::part.adoc[]\n",
            &PreprocessOptions::default(),
            &lookup,
            &NeverCancel,
        ) else {
            panic!("request");
        };
        let response = suspended.request().not_found();
        assert!(matches!(
            suspended.resume(response, &lookup, &cancellation),
            PreprocessStep::Cancelled
        ));
    }

    #[test]
    fn suspended_condition_stack_never_requests_an_unreachable_resource() {
        let mut snapshot = ResourceSnapshot::default();
        snapshot.insert("reachable.adoc", resource("reachable", "included\n"));
        let source = concat!(
            "ifdef::undefined[]\n",
            "include::unreachable.adoc[]\n",
            "endif::[]\n",
            "include::reachable.adoc[]\n",
        );
        let options = PreprocessOptions::default();
        let expected = preprocess(source, &snapshot, &options).expect("one-shot preprocessing");

        let (actual, requests, _) = deferred_preprocess(source, &snapshot, &options);

        assert_eq!(actual.expect("resumable preprocessing"), expected);
        assert_eq!(requests, ["reachable.adoc"]);
    }

    #[test]
    fn optional_absence_is_authoritative_only_after_resume() {
        let snapshot = ResourceSnapshot::default();
        let source = "before\ninclude::missing.adoc[optional]\nafter\n";
        let options = PreprocessOptions::default();
        let expected = preprocess(source, &snapshot, &options).expect("one-shot preprocessing");

        let (actual, requests, _) = deferred_preprocess(source, &snapshot, &options);
        let actual = actual.expect("resumable preprocessing");

        assert_eq!(actual, expected);
        assert_eq!(requests, ["missing.adoc"]);
        assert_eq!(actual.notices.len(), 1);
        assert_eq!(
            actual.notices[0].kind,
            PreprocessNoticeKind::OptionalResourceMissing
        );
    }

    #[test]
    fn repeated_resource_is_acquired_once_but_expanded_each_time() {
        let mut snapshot = ResourceSnapshot::default();
        snapshot.insert("part.adoc", resource("part", "included\n"));
        let source = "include::part.adoc[]\ninclude::part.adoc[]\n";
        let options = PreprocessOptions::default();
        let expected = preprocess(source, &snapshot, &options).expect("one-shot preprocessing");

        let (actual, requests, lookups) = deferred_preprocess(source, &snapshot, &options);

        assert_eq!(actual.expect("resumable preprocessing"), expected);
        assert_eq!(requests, ["part.adoc"]);
        assert_eq!(lookups, 1);
    }

    #[test]
    fn cycle_and_include_limits_do_not_charge_again_after_resume() {
        let mut cycle_snapshot = ResourceSnapshot::default();
        cycle_snapshot.insert("part.adoc", resource("part", "include::part.adoc[]\n"));
        let cycle_source = "include::part.adoc[]\n";
        let cycle_options = PreprocessOptions::default();
        let expected_cycle =
            preprocess(cycle_source, &cycle_snapshot, &cycle_options).expect_err("cycle");

        let (actual_cycle, requests, _) =
            deferred_preprocess(cycle_source, &cycle_snapshot, &cycle_options);

        assert_eq!(actual_cycle.expect_err("resumable cycle"), expected_cycle);
        assert_eq!(requests, ["part.adoc"]);

        let source = "include::one.adoc[]\ninclude::two.adoc[]\ninclude::three.adoc[]\n";
        let snapshot = ["one", "two", "three"]
            .into_iter()
            .map(|name| (format!("{name}.adoc"), resource(name, "")))
            .collect::<ResourceSnapshot>();
        let accepted = PreprocessOptions {
            max_includes: 3,
            ..PreprocessOptions::default()
        };
        let expected = preprocess(source, &snapshot, &accepted).expect("exact include limit");
        let (actual, requests, _) = deferred_preprocess(source, &snapshot, &accepted);
        assert_eq!(actual.expect("resumable exact include limit"), expected);
        assert_eq!(requests.len(), 3);

        let rejected = PreprocessOptions {
            max_includes: 2,
            ..PreprocessOptions::default()
        };
        let expected = preprocess(source, &snapshot, &rejected).expect_err("include limit");
        let (actual, requests, _) = deferred_preprocess(source, &snapshot, &rejected);
        assert_eq!(actual.expect_err("resumable include limit"), expected);
        assert_eq!(requests.len(), 2);
    }

    #[test]
    fn cumulative_node_byte_and_source_map_limits_survive_every_suspension() {
        let source = "include::one.adoc[]\ninclude::two.adoc[]\n";
        let snapshot = [
            ("one.adoc".to_owned(), resource("one", "a\n")),
            ("two.adoc".to_owned(), resource("two", "b\n")),
        ]
        .into_iter()
        .collect::<ResourceSnapshot>();

        for options in [
            PreprocessOptions {
                max_expanded_nodes: 4,
                ..PreprocessOptions::default()
            },
            PreprocessOptions {
                max_total_bytes: 4,
                ..PreprocessOptions::default()
            },
            PreprocessOptions {
                max_source_map_segments: 2,
                ..PreprocessOptions::default()
            },
        ] {
            let expected = preprocess(source, &snapshot, &options).expect("exact limit");
            let (actual, requests, _) = deferred_preprocess(source, &snapshot, &options);
            assert_eq!(actual.expect("resumable exact limit"), expected);
            assert_eq!(requests.len(), 2);
        }

        for options in [
            PreprocessOptions {
                max_expanded_nodes: 3,
                ..PreprocessOptions::default()
            },
            PreprocessOptions {
                max_total_bytes: 3,
                ..PreprocessOptions::default()
            },
            PreprocessOptions {
                max_source_map_segments: 1,
                ..PreprocessOptions::default()
            },
        ] {
            let expected = preprocess(source, &snapshot, &options).expect_err("limit exceeded");
            let (actual, requests, _) = deferred_preprocess(source, &snapshot, &options);
            assert_eq!(actual.expect_err("resumable limit exceeded"), expected);
            assert_eq!(requests.len(), 2);
        }
    }

    #[test]
    fn cancellation_discards_a_suspended_run_without_exposing_partial_output() {
        let mut snapshot = ResourceSnapshot::default();
        snapshot.insert("part.adoc", resource("part", "included\n"));
        let lookup = DeferredLookup {
            snapshot: &snapshot,
            lookups: Cell::new(0),
        };
        let step = preprocess_resumable(
            "prefix\ninclude::part.adoc[]\n",
            &PreprocessOptions::default(),
            &lookup,
            &NeverCancel,
        );
        let PreprocessStep::NeedResource(suspended) = step else {
            panic!("preprocessing must suspend");
        };
        let cancellation = crate::core::CancellationToken::new();
        cancellation.cancel();
        let response = suspended.request().found(resource("part", "included\n"));

        assert!(matches!(
            suspended.resume(response, &lookup, &cancellation),
            PreprocessStep::Cancelled
        ));
    }

    #[test]
    fn resumable_public_state_can_cross_a_worker_boundary() {
        fn assert_send<T: Send>() {}
        fn assert_send_sync<T: Send + Sync>() {}

        assert_send::<PreprocessStep>();
        assert_send::<SuspendedPreprocess>();
        assert_send_sync::<ResourceRequest>();
        assert_send_sync::<ResourceResponse>();
        assert_send_sync::<ResourceLookupResult>();
    }

    #[test]
    fn preprocessing_cancels_at_a_bounded_line_checkpoint() {
        let cancellation = CancelAfter {
            checks: AtomicUsize::new(0),
            completed_checks: 2,
        };
        let source = "paragraph\n".repeat(CHECKPOINT_INTERVAL * 3);

        let failure = preprocess_with(
            &source,
            &ResourceSnapshot::default(),
            &PreprocessOptions::default(),
            PreprocessInputs {
                cancellation: Some(&cancellation),
            },
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
        let actual = preprocess_with(
            source,
            &ResourceSnapshot::default(),
            &PreprocessOptions::default(),
            PreprocessInputs::default(),
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
            preprocess_and_analyze_with(
                &Engine::new(crate::core::AnalysisOptions::default()),
                "paragraph\n",
                &ResourceSnapshot::default(),
                &PreprocessOptions::default(),
                PreprocessInputs {
                    cancellation: Some(&cancellation)
                }
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
        let actual = preprocess_and_analyze_with(
            &engine,
            "include::part.adoc[]\n",
            &snapshot,
            &options,
            PreprocessInputs::default(),
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
        let actual_error = preprocess_and_analyze_with(
            &engine,
            "include::missing.adoc[]\n",
            &snapshot,
            &options,
            PreprocessInputs::default(),
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
