//! Stable, host-independent parsing boundary.
//!
//! Hosts own all I/O and reference resolution. This module only consumes
//! caller-provided text and deterministic options.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::diagnostic::{CoreErrorCode, Diagnostic};
use crate::limits::{ProcessingLimits, SyntaxMode};
use crate::lint::{self, LintConfig};
use crate::parser::{self, AstBlock, ParsedDocument};
use crate::source::{PositionError, SourceDocument};
use crate::syntax::SyntaxTree;

/// Version of the public parsing contract.
pub const CORE_API_VERSION: u16 = 29;
/// Current host-independent syntax and diagnostic behavior profile.
pub const CORE_PROFILE_VERSION: u16 = 17;

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

/// Complete deterministic input to the parsing operation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ParseOptions {
    pub source_id: Option<SourceId>,
    pub syntax_mode: SyntaxMode,
    pub limits: ProcessingLimits,
    /// Host-authoritative values that source text may not change.
    pub protected_attributes: BTreeMap<String, String>,
    pub url_policy: crate::url::UrlPolicy,
}

/// Cooperative cancellation checked at deterministic parsing checkpoints.
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
    profile_version: u16,
    syntax: SyntaxTree,
    ast: parser::AstDocument,
    diagnostics: Vec<Diagnostic>,
}

impl Analysis {
    pub const fn profile_version(&self) -> u16 {
        self.profile_version
    }
    pub const fn source_id(&self) -> Option<&SourceId> {
        self.source_id.as_ref()
    }

    pub const fn syntax(&self) -> &SyntaxTree {
        &self.syntax
    }

    pub const fn ast(&self) -> &parser::AstDocument {
        &self.ast
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    pub fn reference_targets(&self) -> &[crate::document::ReferenceTarget] {
        self.ast.identifiers().targets()
    }

    pub const fn catalogs(&self) -> &crate::catalog::DocumentCatalogs {
        self.ast.catalogs()
    }

    pub const fn structure(&self) -> &crate::structure::DocumentStructure {
        self.ast.structure()
    }

    pub fn references(&self) -> Vec<&crate::inline::Reference> {
        let mut references = Vec::new();
        crate::walker::walk(&self.ast, |node| {
            if let crate::walker::SemanticNode::Inline(crate::inline::Inline::Reference(
                reference,
            )) = node
            {
                references.push(reference);
            }
        });
        references
    }

    pub fn source(&self) -> &str {
        self.syntax.source()
    }

    pub fn source_document(&self) -> &SourceDocument {
        self.syntax.source_document()
    }

    pub fn reference_queries(&self) -> Vec<crate::reference::ReferenceQuery> {
        self.references()
            .into_iter()
            .filter_map(|reference| {
                crate::reference::query_from_reference(self.source_id.clone(), reference)
            })
            .collect()
    }

    pub fn resources(&self) -> Vec<crate::resource::ResourceReference> {
        self.macros()
            .into_iter()
            .filter_map(crate::resource::ResourceReference::from_macro)
            .collect()
    }

    pub fn macros(&self) -> Vec<&crate::inline::StandardMacro> {
        let mut macros = Vec::new();
        crate::walker::walk(&self.ast, |node| {
            if let crate::walker::SemanticNode::Inline(crate::inline::Inline::Macro(node)) = node {
                macros.push(node);
            }
        });
        macros
    }

    pub fn resource_queries(&self) -> Vec<crate::resource::ResourceQuery> {
        self.resources()
            .into_iter()
            .map(|reference| crate::resource::ResourceQuery {
                source_id: self.source_id.clone(),
                reference,
            })
            .collect()
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
            Self::Cancelled => formatter.write_str("parsing was cancelled"),
            Self::InternalInvariant => formatter.write_str("internal parsing invariant failed"),
        }
    }
}

impl Error for ParseError {}

/// Stateless analysis engine with deterministic options.
#[derive(Clone, Debug)]
pub struct Engine {
    options: ParseOptions,
}

impl Engine {
    pub fn new(options: ParseOptions) -> Self {
        Self { options }
    }

    pub fn analyze(&self, source: &str) -> Result<Analysis, ParseError> {
        analyze(source, &self.options)
    }

    pub fn analyze_cancellable(
        &self,
        source: &str,
        cancellation: &dyn CancellationCheck,
    ) -> Result<Analysis, ParseError> {
        analyze_cancellable(source, &self.options, cancellation)
    }
}

/// Analyzes with a cancellation token that never cancels.
pub(crate) fn analyze(source: &str, options: &ParseOptions) -> Result<Analysis, ParseError> {
    analyze_cancellable(source, options, &NeverCancel)
}

/// Analyzes caller-provided source without performing I/O or reference resolution.
pub(crate) fn analyze_cancellable(
    source: &str,
    options: &ParseOptions,
    cancellation: &dyn CancellationCheck,
) -> Result<Analysis, ParseError> {
    analyze_inner(source, options, cancellation)
}

fn analyze_inner(
    source: &str,
    options: &ParseOptions,
    cancellation: &dyn CancellationCheck,
) -> Result<Analysis, ParseError> {
    enforce_limit("input bytes", options.limits.max_input_bytes, source.len())?;

    if cancellation.is_cancelled() {
        return Err(ParseError::Cancelled);
    }

    let shared_source: Arc<str> = Arc::from(source);
    let ParsedDocument { syntax, ast } = parser::parse_shared_cancellable(
        shared_source,
        &parser::ParseConfig {
            max_inline_depth: limit_to_usize(options.limits.max_inline_depth),
            max_list_depth: limit_to_usize(options.limits.max_list_depth),
            max_block_depth: limit_to_usize(options.limits.max_block_depth),
            max_formula_bytes: limit_to_usize(options.limits.max_formula_bytes),
            limits: options.limits,
        },
        &|| cancellation.is_cancelled(),
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
    if options.syntax_mode == SyntaxMode::Strict
        && ast
            .blocks()
            .iter()
            .any(|block| matches!(block, AstBlock::Unsupported(_)))
    {
        return Err(ParseError::UnsupportedSyntax);
    }
    if cancellation.is_cancelled() {
        return Err(ParseError::Cancelled);
    }

    let mut lint_config = LintConfig::default();
    lint_config.max_diagnostics = limit_to_usize(options.limits.max_diagnostics);
    lint_config.max_inline_depth = limit_to_usize(options.limits.max_inline_depth);
    lint_config.max_formula_bytes = limit_to_usize(options.limits.max_formula_bytes);
    lint_config.protected_attributes = options.protected_attributes.clone();
    lint_config.url_policy = options.url_policy.clone();
    lint_config.protected_attribute_severity = if options.syntax_mode == SyntaxMode::Strict {
        crate::diagnostic::Severity::Error
    } else {
        crate::diagnostic::Severity::Warning
    };
    let diagnostics =
        lint::lint_syntax(&syntax, &ast, &lint_config).map_err(ParseError::Position)?;
    if cancellation.is_cancelled() {
        return Err(ParseError::Cancelled);
    }

    Ok(Analysis {
        source_id: options.source_id.clone(),
        profile_version: CORE_PROFILE_VERSION,
        syntax,
        ast,
        diagnostics,
    })
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
    use std::thread;

    use super::{
        CancellationCheck, CancellationToken, ParseError, ParseOptions, SourceId, analyze,
        analyze_cancellable,
    };

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn public_api_is_deterministic_and_source_id_is_opaque() {
        let options = ParseOptions {
            source_id: Some(SourceId::new("host:any/value")),
            ..ParseOptions::default()
        };
        let first = analyze("== 日本語\n", &options).expect("analyze");
        let second = analyze("== 日本語\n", &options).expect("analyze");

        assert_eq!(first.source_id, second.source_id);
        assert_eq!(first.syntax.snapshot(), second.syntax.snapshot());
        assert_eq!(first.ast, second.ast);
        assert_eq!(
            first.source_id.as_ref().map(SourceId::as_str),
            Some("host:any/value")
        );
    }

    #[test]
    fn public_api_accepts_anonymous_sources() {
        let result = analyze("paragraph", &ParseOptions::default()).expect("analyze");
        assert_eq!(result.source_id, None);
    }

    #[test]
    fn analysis_owns_the_source_and_semantic_queries_borrow_the_ast() {
        let analysis = {
            let source = String::from("== 所有される見出し\n");
            analyze(&source, &ParseOptions::default()).expect("analyze")
        };

        assert_eq!(analysis.source(), "== 所有される見出し\n");
        assert_eq!(analysis.syntax().reconstruct(), analysis.source());
        assert_eq!(analysis.source_document().line_count(), 2);
    }

    #[test]
    fn configured_structure_limits_are_enforced() {
        let mut options = ParseOptions::default();
        options.limits.max_blocks = 1;
        assert!(matches!(
            analyze("one\n\ntwo\n", &options),
            Err(ParseError::LimitExceeded {
                resource: "blocks",
                ..
            })
        ));

        options.limits.max_blocks = 100;
        options.limits.max_references = 1;
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

        let mut options = ParseOptions::default();
        options.limits.max_list_depth = 3;
        let analysis = analyze(
            "* one\n** two\n*** three\n**** four\n***** five\n",
            &options,
        )
        .expect("recover deep list");
        let crate::parser::AstBlock::List(list) = &analysis.ast().blocks()[0] else {
            panic!("expected list");
        };
        assert!(depth(list) <= super::limit_to_usize(options.limits.max_list_depth));
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
        let error = analyze_cancellable(&source, &ParseOptions::default(), &cancellation)
            .expect_err("cancelled");
        assert_eq!(error, ParseError::Cancelled);
        assert_eq!(error.code().as_str(), "cancelled");
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
            analyze_cancellable(
                "first\nsecond\nthird\n",
                &ParseOptions::default(),
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
        assert_send_sync::<ParseOptions>();
        assert_send_sync::<CancellationToken>();
        assert_send_sync::<ParseError>();
    }

    #[test]
    fn protected_attribute_is_an_error_in_strict_mode() {
        let mut options = ParseOptions {
            syntax_mode: crate::limits::SyntaxMode::Strict,
            ..ParseOptions::default()
        };
        options.protected_attributes.insert(
            "note-id".to_owned(),
            "123e4567-e89b-12d3-a456-426614174000".to_owned(),
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
    fn public_api_extracts_cross_references_without_resolving_them() {
        let parsed = analyze(
            "[[local]]\n== Local\n\n<<local>> xref:other.adoc#part[] xref:note:123#part[]",
            &ParseOptions::default(),
        )
        .expect("analyze");

        assert_eq!(parsed.references().len(), 3);
        assert_eq!(parsed.reference_targets().len(), 1);
    }

    #[test]
    fn public_api_exposes_resource_queries_without_performing_io() {
        let analysis = analyze(
            "image:https://example.org/a.png[Alt]",
            &ParseOptions::default(),
        )
        .expect("analysis");
        assert_eq!(analysis.resources().len(), 1);
        let queries = analysis.resource_queries();
        assert_eq!(
            queries[0].reference.kind,
            crate::resource::ResourceKind::Image
        );
        assert_eq!(queries[0].reference.target, "https://example.org/a.png");
    }

    #[test]
    fn inline_anchor_macros_join_the_common_reference_target_index() {
        let analysis = analyze(
            "See <<spot>> and anchor:spot[]target.",
            &ParseOptions::default(),
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
        let options = ParseOptions {
            source_id: Some(SourceId::new("opaque:source")),
            ..ParseOptions::default()
        };
        let parsed = analyze(
            "xref:other.adoc#part[] xref:note:123e4567-e89b-12d3-a456-426614174000#part[]",
            &options,
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
    fn public_api_accepts_host_configured_url_schemes() {
        let mut options = ParseOptions::default();
        options
            .url_policy
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
