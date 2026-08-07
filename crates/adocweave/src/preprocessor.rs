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
///
/// Cloning this value preserves its private processing-contract identity.
/// Constructing another value with equal fields creates a distinct contract.
/// Prepared documents can only be analyzed by the originating instance or one
/// of its clones.
#[derive(Clone, Debug)]
pub struct EffectiveProcessingOptions {
    analysis: crate::core::AnalysisOptions,
    preprocess: PreprocessOptions,
    contract: Arc<ProcessingContract>,
}

#[derive(Debug)]
struct ProcessingContract;

impl PartialEq for EffectiveProcessingOptions {
    fn eq(&self, other: &Self) -> bool {
        self.analysis == other.analysis && self.preprocess == other.preprocess
    }
}

impl Eq for EffectiveProcessingOptions {}

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
            contract: Arc::new(ProcessingContract),
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

    /// Returns whether both values belong to the same private contract.
    ///
    /// Equal option fields are not sufficient: only an instance and its clones
    /// share the contract identity.
    pub fn same_contract(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.contract, &other.contract)
    }

    /// Returns equivalent settings with one source identity and a new contract.
    pub fn with_source_id(mut self, source_id: Option<SourceId>) -> Self {
        self.preprocess.source_id = source_id;
        self.contract = Arc::new(ProcessingContract);
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

/// A preprocessed document bound to the effective settings that produced it.
///
/// The private contract prevents a host from preprocessing with one set of
/// shared analysis settings and analyzing the result with another. Only the
/// originating [`EffectiveProcessingOptions`] instance and its clones can
/// analyze this value; a separately constructed equal instance is rejected.
#[derive(Debug)]
pub struct PreparedPreprocessedDocument {
    document: PreprocessedDocument,
    contract: Arc<ProcessingContract>,
}

impl PreparedPreprocessedDocument {
    /// Returns the completed preprocessed document and source map.
    pub const fn document(&self) -> &PreprocessedDocument {
        &self.document
    }
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

/// Failure while analyzing an already prepared document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PreparedAnalysisError {
    /// The document was prepared under a different effective contract.
    ContractMismatch,
    /// Core parsing or analysis failed.
    Parse(ParseError),
    /// Cooperative cancellation discarded the result.
    Cancelled,
}

impl fmt::Display for PreparedAnalysisError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ContractMismatch => formatter.write_str(
                "prepared document belongs to a different effective processing contract",
            ),
            Self::Parse(error) => error.fmt(formatter),
            Self::Cancelled => formatter.write_str("analysis was cancelled"),
        }
    }
}

impl Error for PreparedAnalysisError {}

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

    /// Starts preprocessing under this effective processing contract.
    pub fn preprocess_resumable(
        &self,
        source: &str,
        resources: &(impl ResourceLookup + ?Sized),
        cancellation: &dyn CancellationCheck,
    ) -> EffectivePreprocessStep {
        bind_effective_step(
            preprocess_resumable(source, self.preprocess(), resources, cancellation),
            Arc::clone(&self.contract),
        )
    }

    /// Analyzes a document prepared by this instance or one of its clones.
    ///
    /// A separately constructed options value is rejected even when every
    /// public option field is equal.
    pub fn analyze_preprocessed(
        &self,
        prepared: PreparedPreprocessedDocument,
        inputs: PreprocessInputs<'_>,
    ) -> Result<PreprocessedAnalysis, PreparedAnalysisError> {
        if !Arc::ptr_eq(&self.contract, &prepared.contract) {
            return Err(PreparedAnalysisError::ContractMismatch);
        }
        let cancellation = inputs.cancellation();
        let analysis = Engine::new(self.analysis().clone())
            .analyze_with(
                &prepared.document.source,
                crate::AnalysisInputs {
                    source_id: self.preprocess().source_id.as_ref(),
                    cancellation: Some(cancellation),
                },
            )
            .map_err(|error| {
                if error == ParseError::Cancelled {
                    PreparedAnalysisError::Cancelled
                } else {
                    PreparedAnalysisError::Parse(error)
                }
            })?;
        Ok(PreprocessedAnalysis {
            document: prepared.document,
            analysis,
        })
    }
}

fn preprocess_and_analyze_effective(
    source: &str,
    snapshot: &ResourceSnapshot,
    options: &EffectiveProcessingOptions,
    cancellation: &dyn CancellationCheck,
) -> Result<PreprocessedAnalysis, PreprocessedAnalysisError> {
    let prepared = match options.preprocess_resumable(source, snapshot, cancellation) {
        EffectivePreprocessStep::Complete(document) => document,
        EffectivePreprocessStep::NeedResource(_) => unreachable!("snapshots never defer resources"),
        EffectivePreprocessStep::Failed(error) => {
            return Err(PreprocessedAnalysisError::Preprocess(error));
        }
        EffectivePreprocessStep::HostError(host_error) => {
            return Err(PreprocessedAnalysisError::Preprocess(error(
                PreprocessErrorKind::InternalInvariant,
                options.preprocess().source_id.clone(),
                zero_range(),
                host_error.to_string(),
            )));
        }
        EffectivePreprocessStep::Cancelled => return Err(PreprocessedAnalysisError::Cancelled),
    };
    options
        .analyze_preprocessed(
            prepared,
            PreprocessInputs {
                cancellation: Some(cancellation),
            },
        )
        .map_err(|error| match error {
            PreparedAnalysisError::ContractMismatch => {
                unreachable!("the prepared document uses this effective contract")
            }
            PreparedAnalysisError::Parse(error) => PreprocessedAnalysisError::Parse(error),
            PreparedAnalysisError::Cancelled => PreprocessedAnalysisError::Cancelled,
        })
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

/// Result of preprocessing under one validated effective processing contract.
#[non_exhaustive]
pub enum EffectivePreprocessStep {
    /// Preprocessing completed and the document is ready for matching analysis.
    Complete(PreparedPreprocessedDocument),
    /// Processing needs one authoritative host resource response.
    NeedResource(Box<EffectiveSuspendedPreprocess>),
    /// Processing failed with a deterministic preprocessing error.
    Failed(PreprocessError),
    /// The host failed to satisfy the resource-loading contract.
    HostError(HostResourceError),
    /// Cooperative cancellation discarded all unpublished state.
    Cancelled,
}

/// Opaque continuation bound to one effective processing contract.
pub struct EffectiveSuspendedPreprocess {
    inner: SuspendedPreprocess,
    contract: Arc<ProcessingContract>,
}

impl EffectiveSuspendedPreprocess {
    /// Returns the resource request that must be answered before resuming.
    pub const fn request(&self) -> &ResourceRequest {
        self.inner.request()
    }

    /// Consumes this continuation and resumes under its original contract.
    pub fn resume(
        self,
        response: ResourceResponse,
        resources: &(impl ResourceLookup + ?Sized),
        cancellation: &dyn CancellationCheck,
    ) -> EffectivePreprocessStep {
        let Self { inner, contract } = self;
        bind_effective_step(inner.resume(response, resources, cancellation), contract)
    }
}

fn bind_effective_step(
    step: PreprocessStep,
    contract: Arc<ProcessingContract>,
) -> EffectivePreprocessStep {
    match step {
        PreprocessStep::Complete(document) => {
            EffectivePreprocessStep::Complete(PreparedPreprocessedDocument { document, contract })
        }
        PreprocessStep::NeedResource(suspended) => {
            EffectivePreprocessStep::NeedResource(Box::new(EffectiveSuspendedPreprocess {
                inner: *suspended,
                contract,
            }))
        }
        PreprocessStep::Failed(error) => EffectivePreprocessStep::Failed(error),
        PreprocessStep::HostError(error) => EffectivePreprocessStep::HostError(error),
        PreprocessStep::Cancelled => EffectivePreprocessStep::Cancelled,
    }
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
mod tests;
