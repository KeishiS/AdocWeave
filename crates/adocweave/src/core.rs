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
use crate::parser::{self, AstBlock, CstDocument, ParsedDocument};
use crate::source::{LineIndex, PositionError};

/// Version of the public parsing contract.
pub const CORE_API_VERSION: u16 = 8;
/// Current host-independent syntax and diagnostic behavior profile.
pub const CORE_PROFILE_VERSION: u16 = 2;

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

/// Versioned syntax behavior selected by a host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyntaxProfile {
    pub version: u16,
    pub mode: SyntaxMode,
}

impl Default for SyntaxProfile {
    fn default() -> Self {
        Self {
            version: CORE_PROFILE_VERSION,
            mode: SyntaxMode::Permissive,
        }
    }
}

/// Complete deterministic input to the parsing operation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ParseOptions {
    pub source_id: Option<SourceId>,
    pub profile: SyntaxProfile,
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
    pub source_id: Option<SourceId>,
    pub cst: CstDocument,
    pub ast: parser::AstDocument,
    pub line_index: LineIndex,
    pub diagnostics: Vec<Diagnostic>,
    pub reference_targets: Vec<crate::document::ReferenceTarget>,
    pub references: Vec<crate::inline::Reference>,
}

impl Analysis {
    pub fn source(&self) -> &str {
        self.cst.source()
    }

    pub fn reference_queries(&self) -> Vec<crate::reference::ReferenceQuery> {
        self.references
            .iter()
            .filter_map(|reference| {
                crate::reference::query_from_reference(self.source_id.clone(), reference)
            })
            .collect()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParseError {
    InvalidProfileVersion {
        version: u16,
    },
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
            Self::InvalidProfileVersion { .. } | Self::UnsupportedSyntax => {
                CoreErrorCode::InvalidInput
            }
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
            Self::InvalidProfileVersion { version } => {
                write!(formatter, "unsupported syntax profile version {version}")
            }
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
pub fn analyze(source: &str, options: &ParseOptions) -> Result<Analysis, ParseError> {
    analyze_cancellable(source, options, &NeverCancel)
}

/// Analyzes caller-provided source without performing I/O or reference resolution.
pub fn analyze_cancellable(
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
    if options.profile.version != CORE_PROFILE_VERSION {
        return Err(ParseError::InvalidProfileVersion {
            version: options.profile.version,
        });
    }
    enforce_limit("input bytes", options.limits.max_input_bytes, source.len())?;

    let mut line_start = 0;
    for (index, byte) in source.bytes().enumerate() {
        if index % 4096 == 0 && cancellation.is_cancelled() {
            return Err(ParseError::Cancelled);
        }
        if byte == b'\n' {
            let end = if index > line_start && source.as_bytes()[index - 1] == b'\r' {
                index - 1
            } else {
                index
            };
            enforce_limit(
                "line bytes",
                options.limits.max_line_bytes,
                end - line_start,
            )?;
            line_start = index + 1;
        }
    }
    enforce_limit(
        "line bytes",
        options.limits.max_line_bytes,
        source.len() - line_start,
    )?;
    if cancellation.is_cancelled() {
        return Err(ParseError::Cancelled);
    }

    let shared_source: Arc<str> = Arc::from(source);
    let ParsedDocument { cst, ast } = parser::parse_shared_cancellable(
        Arc::clone(&shared_source),
        &parser::ParseConfig {
            max_inline_depth: limit_to_usize(options.limits.max_inline_depth),
            max_list_depth: limit_to_usize(options.limits.max_list_depth),
            max_formula_bytes: limit_to_usize(options.limits.max_formula_bytes),
            limits: options.limits,
        },
        &|| cancellation.is_cancelled(),
    )
    .map_err(|failure| match failure {
        parser::ParseFailure::Position(error) => ParseError::Position(error),
        parser::ParseFailure::Budget(error) => ParseError::LimitExceeded {
            resource: error.resource,
            limit: error.limit,
            actual: error.actual,
        },
        parser::ParseFailure::Cancelled => ParseError::Cancelled,
    })?;
    if options.profile.mode == SyntaxMode::Strict
        && ast
            .blocks
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
    lint_config.protected_attribute_severity = if options.profile.mode == SyntaxMode::Strict {
        crate::diagnostic::Severity::Error
    } else {
        crate::diagnostic::Severity::Warning
    };
    let diagnostics = lint::lint_cst(&cst, &ast, &lint_config).map_err(ParseError::Position)?;
    let reference_targets = crate::document::reference_targets(&ast);
    let references = collect_references(&ast);
    if cancellation.is_cancelled() {
        return Err(ParseError::Cancelled);
    }

    let line_index = LineIndex::from_shared(shared_source).map_err(ParseError::Position)?;
    Ok(Analysis {
        source_id: options.source_id.clone(),
        cst,
        ast,
        line_index,
        diagnostics,
        reference_targets,
        references,
    })
}

fn collect_references(document: &parser::AstDocument) -> Vec<crate::inline::Reference> {
    fn collect(inlines: &[crate::inline::Inline], output: &mut Vec<crate::inline::Reference>) {
        for inline in inlines {
            match inline {
                crate::inline::Inline::Reference(reference) => {
                    output.push(reference.clone());
                    collect(&reference.label, output);
                }
                crate::inline::Inline::Link(link) => collect(&link.label, output),
                crate::inline::Inline::Styled { children, .. } => collect(children, output),
                crate::inline::Inline::Text(_)
                | crate::inline::Inline::Literal { .. }
                | crate::inline::Inline::AttributeReference { .. }
                | crate::inline::Inline::Formula(_) => {}
            }
        }
    }
    let mut output = Vec::new();
    document.visit_inline_sequences(|inlines| collect(inlines, &mut output));
    output
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
        CancellationCheck, CancellationToken, ParseError, ParseOptions, SourceId, SyntaxProfile,
        analyze, analyze_cancellable,
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
        assert_eq!(first.cst.snapshot(), second.cst.snapshot());
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
    fn analysis_owns_the_source_and_all_indexes_share_that_snapshot() {
        let analysis = {
            let source = String::from("== 所有される見出し\n");
            analyze(&source, &ParseOptions::default()).expect("analyze")
        };

        assert_eq!(analysis.source(), "== 所有される見出し\n");
        assert_eq!(analysis.cst.reconstruct(), analysis.source());
        assert_eq!(analysis.line_index.line_count(), 2);
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
        let crate::parser::AstBlock::List(list) = &analysis.ast.blocks[0] else {
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
        assert_send_sync::<SyntaxProfile>();
        assert_send_sync::<ParseOptions>();
        assert_send_sync::<CancellationToken>();
        assert_send_sync::<ParseError>();
    }

    #[test]
    fn protected_attribute_is_an_error_in_strict_mode() {
        let mut options = ParseOptions::default();
        options.profile.mode = crate::limits::SyntaxMode::Strict;
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

        assert_eq!(parsed.references.len(), 3);
        assert_eq!(parsed.reference_targets.len(), 1);
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
