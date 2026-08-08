//! Pure preprocessing over caller-provided resource snapshots.

mod directive;
mod expansion;
mod include;
mod machine;
mod projection;
mod source_map;

use include::*;
pub use include::{IncludeRequest, discover_includes, resolve_include_target};
use machine::*;

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
pub(crate) enum PreprocessStep {
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
pub(crate) struct SuspendedPreprocess {
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
pub(crate) fn preprocess_resumable(
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
