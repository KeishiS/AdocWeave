//! Stable, host-independent parsing boundary.
//!
//! Hosts own all I/O and reference resolution. This module only consumes
//! caller-provided text and deterministic options.

use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::diagnostic::{CoreErrorCode, Diagnostic};
use crate::limits::{AnalysisLimits, SyntaxMode};
use crate::lint::{self, LintConfig};
use crate::parser::{self, AstBlock, ParsedDocument};
use crate::source::{PositionError, SourceDocument};
use crate::syntax::SyntaxTree;

/// A caller-defined, opaque source identity.
///
/// AdocWeave never interprets this value as a path, URL, UUID, or database key.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub struct SourceId(String);

impl SourceId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Deterministic settings for syntax recognition and resource budgets.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SyntaxOptions {
    pub syntax_mode: SyntaxMode,
    pub limits: AnalysisLimits,
}

/// Diagnostic rules applied to the parsed snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticProfile {
    pub lint: LintConfig,
}

impl Default for DiagnosticProfile {
    fn default() -> Self {
        let mut lint = LintConfig::default();
        lint.set_rule(
            crate::lint::PROTECTED_ATTRIBUTE,
            crate::lint::RuleSettings {
                enabled: true,
                severity: crate::diagnostic::Severity::Warning,
            },
        );
        Self { lint }
    }
}

/// Deterministic configuration shared by analyses.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AnalysisOptions {
    pub syntax: SyntaxOptions,
    pub diagnostics: DiagnosticProfile,
    pub attributes: crate::attributes::ExternalAttributes,
}

/// Cooperative cancellation checked at deterministic checkpoints throughout analysis.
pub trait CancellationCheck: Send + Sync {
    fn is_cancelled(&self) -> bool;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NeverCancel;

impl CancellationCheck for NeverCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
}

#[derive(Debug, Default)]
pub struct CancellationToken(AtomicBool);

impl CancellationToken {
    pub const fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
}

impl CancellationCheck for CancellationToken {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// Owned output of one analysis. Every consumer must use this same snapshot.
#[derive(Debug)]
pub struct Analysis {
    source_id: Option<SourceId>,
    package_version: &'static str,
    syntax: SyntaxTree,
    document: crate::document::Document,
    diagnostics: Vec<Diagnostic>,
}

impl Analysis {
    /// Exact package SemVer that produced this analysis.
    pub const fn package_version(&self) -> &'static str {
        self.package_version
    }
    pub const fn source_id(&self) -> Option<&SourceId> {
        self.source_id.as_ref()
    }

    pub const fn syntax(&self) -> &SyntaxTree {
        &self.syntax
    }

    /// Returns the immutable public semantic document model.
    pub const fn document(&self) -> &crate::document::Document {
        &self.document
    }

    pub(crate) const fn ast(&self) -> &parser::AstDocument {
        self.document.inner()
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    pub fn reference_targets(&self) -> &[crate::document::ReferenceTarget] {
        self.ast().identifiers().targets()
    }

    pub const fn catalogs(&self) -> &crate::catalog::DocumentCatalogs {
        self.ast().catalogs()
    }

    pub const fn structure(&self) -> &crate::structure::DocumentStructure {
        self.ast().structure()
    }

    pub const fn presentation(&self) -> &crate::presentation::DocumentPresentation {
        self.ast().presentation()
    }

    /// Returns the source-ordered document attribute environment.
    pub const fn attribute_environment(&self) -> &crate::attributes::AttributeEnvironment {
        self.ast().attribute_environment()
    }

    pub const fn layout(&self) -> &crate::presentation::DocumentLayout {
        self.ast().layout()
    }

    /// Returns immutable, source-ordered facts collected during analysis.
    pub const fn facts(&self) -> &crate::resolved::DocumentFacts {
        self.ast().resolved.facts()
    }

    /// Returns every attribute reference with its position-dependent binding.
    pub fn attribute_references(&self) -> &[crate::attributes::AttributeReference] {
        self.facts().attribute_references()
    }

    pub fn attribute_query_product(&self) -> crate::attributes::AttributeQueryProduct {
        crate::attributes::AttributeQueryProduct {
            bindings: self.attribute_environment().bindings().to_vec(),
            references: self.attribute_references().to_vec(),
        }
    }

    /// Returns standard document-attribute occurrences in source order.
    ///
    /// Unlike [`Self::presentation`], this preserves duplicates, set/unset
    /// operations, empty values, and source ranges for host-side editing or
    /// metadata projection.
    pub fn document_attribute_occurrences(
        &self,
    ) -> &[crate::attributes::DocumentAttributeOccurrence] {
        self.document.attribute_occurrences()
    }

    /// Returns leading document-header attribute occurrences in source order.
    pub fn header_attribute_occurrences(
        &self,
    ) -> &[crate::attributes::DocumentAttributeOccurrence] {
        self.document.header_attribute_occurrences()
    }

    pub fn references(&self) -> &[crate::inline::Reference] {
        self.facts().references()
    }

    /// Returns source-ordered authored links without resolving local files or URLs.
    pub fn links(&self) -> &[crate::inline::Link] {
        self.facts().links()
    }

    pub fn source(&self) -> &str {
        self.syntax.source()
    }

    pub fn source_document(&self) -> &SourceDocument {
        self.syntax.source_document()
    }

    pub fn reference_queries(&self) -> Vec<crate::reference::ReferenceQuery> {
        self.references()
            .iter()
            .filter_map(|reference| {
                crate::reference::query_from_reference(self.source_id.clone(), reference)
            })
            .collect()
    }

    pub fn resources(&self) -> &[crate::resource::ResourceReference] {
        self.facts().resources()
    }

    pub fn macros(&self) -> &[crate::inline::StandardMacro] {
        self.facts().macros()
    }

    /// Returns source-ordered citations of external bibliography entries.
    ///
    /// AdocWeave does not resolve citation keys. The host owns the bibliography
    /// library and decides how each key becomes a display string.
    pub fn citations(&self) -> Vec<crate::citation::Citation> {
        crate::citation::citations(self.macros())
    }

    pub fn resource_queries(&self) -> Vec<crate::resource::ResourceQuery> {
        self.resources()
            .iter()
            .cloned()
            .map(|reference| crate::resource::ResourceQuery {
                source_id: self.source_id.clone(),
                reference,
            })
            .collect()
    }

    /// Returns source-ordered relative file candidates without performing I/O.
    pub fn local_targets(&self) -> Vec<crate::local_target::LocalTargetReference> {
        let mut targets = self
            .links()
            .iter()
            .filter_map(crate::local_target::LocalTargetReference::from_link)
            .chain(
                self.references()
                    .iter()
                    .filter_map(crate::local_target::LocalTargetReference::from_reference),
            )
            .chain(
                self.resources()
                    .iter()
                    .filter_map(crate::local_target::LocalTargetReference::from_resource),
            )
            .collect::<Vec<_>>();
        targets.sort_by_key(|target| target.range.start());
        targets
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParseError {
    LimitExceeded {
        resource: &'static str,
        limit: u32,
        actual: u64,
    },
    Position(PositionError),
    UnsupportedSyntax,
    Cancelled,
    InternalInvariant,
}

impl ParseError {
    pub const fn code(&self) -> CoreErrorCode {
        match self {
            Self::UnsupportedSyntax => CoreErrorCode::InvalidInput,
            Self::LimitExceeded { .. } => CoreErrorCode::LimitExceeded,
            Self::Position(_) => CoreErrorCode::ParseFailed,
            Self::Cancelled => CoreErrorCode::Cancelled,
            Self::InternalInvariant => CoreErrorCode::InternalInvariant,
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LimitExceeded {
                resource,
                limit,
                actual,
            } => write!(
                formatter,
                "{resource} limit exceeded (limit {limit}, actual {actual})"
            ),
            Self::Position(error) => error.fmt(formatter),
            Self::UnsupportedSyntax => {
                formatter.write_str("unsupported syntax is forbidden in strict mode")
            }
            Self::Cancelled => formatter.write_str("analysis was cancelled"),
            Self::InternalInvariant => formatter.write_str("internal parsing invariant failed"),
        }
    }
}

impl Error for ParseError {}

/// Optional inputs for one analysis.
///
/// Every field defaults to absent, so callers name only what they need:
/// `AnalysisInputs { source_id: Some(&id), ..AnalysisInputs::default() }`.
#[derive(Default)]
pub struct AnalysisInputs<'inputs> {
    /// Identity recorded on the analysis and its diagnostics.
    pub source_id: Option<&'inputs SourceId>,
    /// Cooperative cancellation checked at bounded checkpoints.
    pub cancellation: Option<&'inputs dyn CancellationCheck>,
}

/// Stateless analysis engine with deterministic options.
#[derive(Clone, Debug)]
pub struct Engine {
    options: AnalysisOptions,
}

impl Engine {
    pub fn new(options: AnalysisOptions) -> Self {
        Self { options }
    }

    pub(crate) const fn options(&self) -> &AnalysisOptions {
        &self.options
    }

    pub fn analyze(&self, source: &str) -> Result<Analysis, ParseError> {
        analyze(source, &self.options)
    }

    /// Analyzes `source` with inputs beyond the engine options.
    ///
    /// [`Engine::analyze`] covers the common case. This entry takes the optional
    /// inputs as one value so that a new input becomes a field instead of a new
    /// method for every combination.
    pub fn analyze_with(
        &self,
        source: &str,
        inputs: AnalysisInputs<'_>,
    ) -> Result<Analysis, ParseError> {
        analyze_cancellable_with_source_id(
            source,
            inputs.source_id,
            &self.options,
            inputs.cancellation.unwrap_or(&NeverCancel),
        )
    }
}

/// Analyzes with a cancellation token that never cancels.
pub(crate) fn analyze(source: &str, options: &AnalysisOptions) -> Result<Analysis, ParseError> {
    analyze_cancellable_with_source_id(source, None, options, &NeverCancel)
}

fn analyze_cancellable_with_source_id(
    source: &str,
    source_id: Option<&SourceId>,
    options: &AnalysisOptions,
    cancellation: &dyn CancellationCheck,
) -> Result<Analysis, ParseError> {
    enforce_limit(
        "input bytes",
        options.syntax.limits.max_input_bytes,
        source.len(),
    )?;

    if cancellation.is_cancelled() {
        return Err(ParseError::Cancelled);
    }

    let shared_source: Arc<str> = Arc::from(source);
    let ParsedDocument { syntax, ast } = parser::parse_shared_cancellable(
        shared_source,
        &parser::ParseConfig {
            max_inline_depth: limit_to_usize(options.syntax.limits.max_inline_depth),
            max_list_depth: limit_to_usize(options.syntax.limits.max_list_depth),
            max_block_depth: limit_to_usize(options.syntax.limits.max_block_depth),
            max_formula_bytes: limit_to_usize(options.syntax.limits.max_formula_bytes),
            limits: options.syntax.limits,
        },
        &options.attributes,
        cancellation,
    )
    .map_err(|failure| match failure {
        crate::parser_support::ParseFailure::Position(error) => ParseError::Position(error),
        crate::parser_support::ParseFailure::Budget(error) => ParseError::LimitExceeded {
            resource: error.resource,
            limit: error.limit,
            actual: error.actual,
        },
        crate::parser_support::ParseFailure::Cancelled => ParseError::Cancelled,
        crate::parser_support::ParseFailure::InternalInvariant => ParseError::InternalInvariant,
    })?;
    if options.syntax.syntax_mode == SyntaxMode::Strict {
        enforce_strict_syntax(&ast, cancellation)?;
    }
    if cancellation.is_cancelled() {
        return Err(ParseError::Cancelled);
    }

    let mut lint_config = options.diagnostics.lint.clone();
    lint_config
        .protected_attributes
        .extend(options.attributes.clone());
    let diagnostics = lint::lint_parsed_document_cancellable(
        lint::LintContext::new(&syntax, &ast),
        &lint_config,
        cancellation,
    )
    .map_err(|error| match error {
        lint::LintError::Position(error) => ParseError::Position(error),
        lint::LintError::Cancelled => ParseError::Cancelled,
    })?;
    if cancellation.is_cancelled() {
        return Err(ParseError::Cancelled);
    }

    Ok(Analysis {
        source_id: source_id.cloned(),
        package_version: crate::VERSION,
        syntax,
        document: crate::document::Document::from_ast(ast),
        diagnostics,
    })
}

fn enforce_strict_syntax(
    document: &crate::parser::AstDocument,
    cancellation: &dyn CancellationCheck,
) -> Result<(), ParseError> {
    let mut checkpoint = crate::cancellation::CancellationCheckpoint::new(cancellation);
    match has_unsupported_syntax(document, &mut checkpoint) {
        Ok(true) => Err(ParseError::UnsupportedSyntax),
        Ok(false) => Ok(()),
        Err(()) => Err(ParseError::Cancelled),
    }
}

fn has_unsupported_syntax(
    document: &crate::parser::AstDocument,
    checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
) -> Result<bool, ()> {
    let result = crate::walker::try_walk_block_slice(document.blocks(), |node| {
        if checkpoint.is_cancelled() {
            return std::ops::ControlFlow::Break(Err(()));
        }
        // A directive is supported syntax that this analysis did not evaluate,
        // so the document stays processable: refusing it here would refuse
        // every document that writes `include::` and is analyzed on its own.
        if matches!(
            node,
            crate::walker::SemanticNode::Block(AstBlock::Unsupported(block))
                if block.kind == crate::block_model::UnsupportedKind::Syntax
        ) {
            return std::ops::ControlFlow::Break(Ok(true));
        }
        std::ops::ControlFlow::Continue(())
    });
    match result {
        std::ops::ControlFlow::Break(result) => result,
        std::ops::ControlFlow::Continue(()) => Ok(false),
    }
}

fn enforce_limit(resource: &'static str, limit: u32, actual: usize) -> Result<(), ParseError> {
    if actual > limit_to_usize(limit) {
        Err(ParseError::LimitExceeded {
            resource,
            limit,
            actual: u64::try_from(actual).expect("usize fits u64 on supported targets"),
        })
    } else {
        Ok(())
    }
}

fn limit_to_usize(limit: u32) -> usize {
    usize::try_from(limit).expect("u32 fits usize on supported targets")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    use super::{
        AnalysisInputs, AnalysisOptions, CancellationCheck, CancellationToken, Engine, ParseError,
        SourceId, SyntaxOptions, analyze, analyze_cancellable_with_source_id,
    };

    #[test]
    fn strict_mode_scan_cancels_after_parser_and_lowering() {
        struct CancelAfterFirstCheckpoint(AtomicUsize);

        impl CancellationCheck for CancelAfterFirstCheckpoint {
            fn is_cancelled(&self) -> bool {
                self.0.fetch_add(1, Ordering::Relaxed) >= 1
            }
        }

        let source = (0..crate::cancellation::CHECKPOINT_INTERVAL * 2)
            .map(|index| format!("paragraph {index}\n\n"))
            .collect::<String>();
        let parsed = crate::parser::parse(&source).expect("parse");
        let cancellation = CancelAfterFirstCheckpoint(AtomicUsize::new(0));

        let result = super::enforce_strict_syntax(&parsed.ast, &cancellation);

        assert_eq!(result, Err(ParseError::Cancelled));
        assert_eq!(cancellation.0.load(Ordering::Relaxed), 2);
    }

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn public_api_is_deterministic_and_source_id_is_opaque() {
        let engine = Engine::new(AnalysisOptions::default());
        let first = engine
            .analyze_with(
                "== 日本語\n",
                AnalysisInputs {
                    source_id: Some(&SourceId::new("host:any/value")),
                    ..AnalysisInputs::default()
                },
            )
            .expect("analyze");
        let second = engine
            .analyze_with(
                "== 日本語\n",
                AnalysisInputs {
                    source_id: Some(&SourceId::new("host:any/value")),
                    ..AnalysisInputs::default()
                },
            )
            .expect("analyze");

        assert_eq!(first.source_id, second.source_id);
        assert_eq!(first.syntax.snapshot(), second.syntax.snapshot());
        assert_eq!(first.document, second.document);
        assert_eq!(
            first.source_id.as_ref().map(SourceId::as_str),
            Some("host:any/value")
        );
    }

    #[test]
    fn public_api_accepts_anonymous_sources() {
        let result = analyze("paragraph", &AnalysisOptions::default()).expect("analyze");
        assert_eq!(result.source_id, None);
    }

    #[test]
    fn analysis_owns_the_source_and_semantic_queries_borrow_the_ast() {
        let analysis = {
            let source = String::from("== 所有される見出し\n");
            analyze(&source, &AnalysisOptions::default()).expect("analyze")
        };

        assert_eq!(analysis.source(), "== 所有される見出し\n");
        assert_eq!(analysis.syntax().reconstruct(), analysis.source());
        assert_eq!(analysis.source_document().line_count(), 2);
    }

    #[test]
    fn configured_structure_limits_are_enforced() {
        let mut options = AnalysisOptions::default();
        options.syntax.limits.max_blocks = 1;
        assert!(matches!(
            analyze("one\n\ntwo\n", &options),
            Err(ParseError::LimitExceeded {
                resource: "blocks",
                ..
            })
        ));

        options.syntax.limits.max_blocks = 100;
        options.syntax.limits.max_references = 1;
        assert!(matches!(
            analyze("xref:a.adoc[] xref:b.adoc[]", &options),
            Err(ParseError::LimitExceeded {
                resource: "references",
                ..
            })
        ));
    }

    #[test]
    fn list_tree_is_capped_at_the_configured_depth() {
        fn depth(list: &crate::parser::ListBlock) -> usize {
            1 + list
                .items
                .iter()
                .flat_map(|item| &item.children)
                .map(depth)
                .max()
                .unwrap_or(0)
        }

        let mut options = AnalysisOptions::default();
        options.syntax.limits.max_list_depth = 3;
        let analysis = analyze(
            "* one\n** two\n*** three\n**** four\n***** five\n",
            &options,
        )
        .expect("recover deep list");
        let crate::parser::AstBlock::List(list) = &analysis.ast().blocks()[0] else {
            panic!("expected list");
        };
        assert!(depth(list) <= super::limit_to_usize(options.syntax.limits.max_list_depth));
        assert!(
            analysis
                .diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.code.as_str() == "inconsistent-list" })
        );
    }

    #[test]
    fn cancellation_is_reported_with_stable_code() {
        struct CancelAfterFirstCheck(std::sync::atomic::AtomicUsize);
        impl CancellationCheck for CancelAfterFirstCheck {
            fn is_cancelled(&self) -> bool {
                self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed) > 0
            }
        }

        let source = "a".repeat(16 * 1024);
        let cancellation = CancelAfterFirstCheck(std::sync::atomic::AtomicUsize::new(0));
        let error = analyze_cancellable_with_source_id(
            &source,
            None,
            &AnalysisOptions::default(),
            &cancellation,
        )
        .expect_err("cancelled");
        assert_eq!(error, ParseError::Cancelled);
        assert_eq!(error.code().as_str(), "cancelled");
        assert_eq!(error.to_string(), "analysis was cancelled");
    }

    #[test]
    fn cancellation_is_checked_inside_the_block_parser_loop() {
        struct CancelDuringParser(std::sync::atomic::AtomicUsize);
        impl CancellationCheck for CancelDuringParser {
            fn is_cancelled(&self) -> bool {
                self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed) >= 3
            }
        }

        let cancellation = CancelDuringParser(std::sync::atomic::AtomicUsize::new(0));
        assert!(matches!(
            analyze_cancellable_with_source_id(
                "first\nsecond\nthird\n",
                None,
                &AnalysisOptions::default(),
                &cancellation,
            ),
            Err(ParseError::Cancelled)
        ));
    }

    #[test]
    fn cancellation_token_can_be_shared_across_threads() {
        let token = Arc::new(CancellationToken::new());
        let other = Arc::clone(&token);
        thread::spawn(move || other.cancel())
            .join()
            .expect("thread");
        assert!(token.is_cancelled());
    }

    #[test]
    fn public_types_are_send_and_sync() {
        assert_send_sync::<SourceId>();
        assert_send_sync::<AnalysisOptions>();
        assert_send_sync::<CancellationToken>();
        assert_send_sync::<ParseError>();
    }

    #[test]
    fn protected_attribute_is_a_warning_in_the_default_analysis_profile() {
        let mut options = AnalysisOptions::default();
        options.diagnostics.lint.protected_attributes.insert(
            "note-id".to_owned(),
            Some("123e4567-e89b-12d3-a456-426614174000".to_owned()),
        );
        let result = analyze(
            "= Note\n:note-id: 00000000-0000-0000-0000-000000000000\n",
            &options,
        )
        .expect("analysis recovers with diagnostic");
        assert!(result.diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_str() == "protected-attribute"
                && diagnostic.severity == crate::diagnostic::Severity::Warning
        }));
    }

    #[test]
    fn protected_attribute_is_an_error_in_strict_mode() {
        let mut options = AnalysisOptions {
            syntax: SyntaxOptions {
                syntax_mode: crate::limits::SyntaxMode::Strict,
                ..SyntaxOptions::default()
            },
            ..AnalysisOptions::default()
        };
        options.diagnostics.lint.set_rule(
            crate::lint::PROTECTED_ATTRIBUTE,
            crate::lint::RuleSettings {
                enabled: true,
                severity: crate::diagnostic::Severity::Error,
            },
        );
        options.diagnostics.lint.protected_attributes.insert(
            "note-id".to_owned(),
            Some("123e4567-e89b-12d3-a456-426614174000".to_owned()),
        );
        let result = analyze(
            "= Note\n:note-id: 00000000-0000-0000-0000-000000000000\n",
            &options,
        )
        .expect("analysis recovers with diagnostic");
        assert!(result.diagnostics.iter().any(|diagnostic| {
            diagnostic.code.as_str() == "protected-attribute"
                && diagnostic.severity == crate::diagnostic::Severity::Error
        }));
    }

    #[test]
    fn incomplete_table_header_candidate_recovers_in_both_syntax_modes() {
        let source = "[format=csv]\n|===\nname,\"open\n\ncontinued\n|===\n";
        for syntax_mode in [
            crate::limits::SyntaxMode::Permissive,
            crate::limits::SyntaxMode::Strict,
        ] {
            let analysis = analyze(
                source,
                &AnalysisOptions {
                    syntax: SyntaxOptions {
                        syntax_mode,
                        ..SyntaxOptions::default()
                    },
                    ..AnalysisOptions::default()
                },
            )
            .expect("analysis recovers from an unclosed quoted cell");
            let crate::parser::AstBlock::Delimited(block) = &analysis.ast().blocks()[0] else {
                panic!("expected table");
            };
            let crate::parser::DelimitedContent::Table(table) = &block.content else {
                panic!("expected typed table");
            };
            assert_eq!(table.rows[0].section, crate::table::TableSection::Body);
            assert!(analysis.diagnostics().iter().any(|diagnostic| {
                diagnostic.code.as_str() == "invalid-table"
                    && diagnostic.message == "unclosed quoted table cell"
            }));
        }
    }

    #[test]
    fn public_api_extracts_cross_references_without_resolving_them() {
        let parsed = analyze(
            "[[local]]\n== Local\n\n<<local>> xref:other.adoc#part[] xref:note:123#part[]",
            &AnalysisOptions::default(),
        )
        .expect("analyze");

        assert_eq!(parsed.references().len(), 3);
        assert_eq!(parsed.reference_targets().len(), 1);
    }

    #[test]
    fn public_api_exposes_resource_queries_without_performing_io() {
        let analysis = analyze(
            "image:https://example.org/a.png[Alt]\n\n\
             video:https://example.org/demo.mp4[Demo,poster=https://example.org/poster.jpg]",
            &AnalysisOptions::default(),
        )
        .expect("analysis");
        assert_eq!(analysis.resources().len(), 3);
        let queries = analysis.resource_queries();
        assert_eq!(
            queries[0].reference.purpose(),
            crate::resource::ResourcePurpose::Image
        );
        assert_eq!(queries[0].reference.target(), "https://example.org/a.png");
        assert_eq!(
            queries[1].reference.purpose(),
            crate::resource::ResourcePurpose::Video
        );
        assert_eq!(
            queries[2].reference.purpose(),
            crate::resource::ResourcePurpose::VideoPoster
        );
        assert_eq!(
            queries[2].reference.target(),
            "https://example.org/poster.jpg"
        );
        assert_eq!(
            queries[2].reference.owner_range(),
            queries[1].reference.owner_range()
        );
        assert_ne!(queries[2].reference.range(), queries[1].reference.range());
    }

    #[test]
    fn inline_anchor_macros_join_the_common_reference_target_index() {
        let analysis = analyze(
            "See <<spot>> and anchor:spot[]target.",
            &AnalysisOptions::default(),
        )
        .expect("analysis");
        assert!(analysis.reference_targets().iter().any(|target| {
            target.kind == crate::document::ReferenceTargetKind::InlineAnchor && target.id == "spot"
        }));
        assert!(
            !analysis
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.code.as_str() == "unresolved-cross-reference")
        );
    }

    #[test]
    fn reference_resolution_queries_are_host_independent() {
        let parsed = Engine::new(AnalysisOptions::default())
            .analyze_with(
                "xref:other.adoc#part[] xref:note:123e4567-e89b-12d3-a456-426614174000#part[]",
                AnalysisInputs {
                    source_id: Some(&SourceId::new("opaque:source")),
                    ..AnalysisInputs::default()
                },
            )
            .expect("analyze");
        let queries = parsed.reference_queries();

        assert_eq!(queries.len(), 2);
        assert_eq!(
            queries[0].source_id.as_ref().map(SourceId::as_str),
            Some("opaque:source")
        );
        assert!(matches!(
            queries[1].target,
            crate::reference::ReferenceKey::Scheme {
                ref scheme,
                ref locator,
                ..
            } if scheme == "note" && locator == "123e4567-e89b-12d3-a456-426614174000"
        ));
    }

    #[test]
    fn reference_queries_use_the_expanded_semantic_target() {
        let analysis = analyze(
            "= Title\n:document: other\n\nxref:{document}.adoc#part[]",
            &AnalysisOptions::default(),
        )
        .expect("analyze");
        let reference = &analysis.references()[0];

        assert!(matches!(
            reference.authored_destination,
            crate::inline::ReferenceDestination::Document {
                ref document,
                ..
            } if document == "{document}.adoc"
        ));
        assert!(matches!(
            reference.target,
            Some(crate::reference::ReferenceKey::Document {
                ref document,
                anchor: Some(ref anchor),
            }) if document == "other.adoc" && anchor == "part"
        ));
        assert_eq!(
            analysis.reference_queries()[0].target,
            reference.target.clone().expect("semantic target")
        );
    }

    #[test]
    fn public_api_accepts_host_configured_url_schemes() {
        let mut options = AnalysisOptions::default();
        options
            .diagnostics
            .lint
            .authored_url_policy
            .allowed_schemes
            .insert("mailto".to_owned());
        let parsed = analyze("mailto:user@example.com[mail]", &options).expect("analyze");

        assert!(
            !parsed
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code.as_str() == "invalid-url-scheme")
        );
    }
}
