//! Core application boundary for AdocWeave.
//!
//! The command-line interface is a host adapter around this API and owns file
//! and standard-stream I/O. Parsing, diagnostics, formatting, and rendering
//! remain deterministic core operations over caller-provided input.

mod ast_util;
mod attributes;
mod block_grammar;
mod block_model;
mod block_sequence;
mod budget;
mod cancellation;
mod catalog;
mod citation;
mod cjk;
mod conformance;
mod core;
mod delimiter;
mod diagnostic;
mod document;
mod document_header;
mod execution;
mod formatter;
mod generated_bibliography;
mod html;
mod inline;
mod inline_model;
mod json;
mod limits;
mod lint;
mod list_parser;
mod local_target;
mod lowering;
mod parser;
mod parser_support;
mod preprocessor;
mod presentation;
mod projection;
mod reference;
mod render;
mod resolved;
mod resource;
mod source;
mod structure;
mod substitution;
mod syntax;
mod syntax_builder;
mod syntax_diagnostics;
mod table;
mod url;
mod walker;

/// Typed semantic document model and output-independent queries.
pub mod semantic {
    pub use crate::attributes::{
        AttributeBinding, AttributeBindingId, AttributeEnvironment, AttributeEventId,
        AttributePosition, AttributeQueryProduct, AttributeReference, AttributeValueContinuation,
        DocumentAttributeContinuation, DocumentAttributeOccurrence, DocumentAttributeOperation,
        DocumentAttributeValue, DocumentAttributeValueLine, ExternalAttributes, ResolvedAttribute,
    };
    pub use crate::block_model::{
        AdmonitionKind, AdmonitionPresentation, Author, Block, BlockMetadata, BlockProblem,
        BlockProblemKind, BlockTitle, BreakBlock, BreakKind, CalloutMarker, ChecklistState,
        DelimitedBlock, DelimitedBlockKind, DelimitedContent, DelimitedPresentation,
        DescriptionTerm, DocumentHeader, DocumentType, ElementAttribute, ExplicitAnchor, Heading,
        HeadingKind, HeadingProblem, ListBlock, ListItem, ListKind, ListPresentationProblem,
        ListPresentationProblemKind, ListProblem, ListProblemKind, LiteralParagraph, MathBlock,
        MathProblem, MathProblemKind, MetadataValue, OrderedListPresentation, OrderedListStyle,
        Paragraph, QuoteKind, QuotePresentation, Revision, SourceBlock, SourceInfo, Unsupported,
        UnsupportedKind, VerbatimBlock, VerbatimKind,
    };
    pub use crate::catalog::{
        BibliographyEntry, BibliographyReference, CatalogProblem, CatalogProblemKind,
        DocumentCatalogs, Footnote, FootnoteOccurrence, IndexEntry,
    };
    pub use crate::citation::{Citation, CitationKey};
    pub use crate::document::{
        Document, DocumentElement, DocumentIdentifiers, DocumentSymbol, HeadingId, ReferenceTarget,
        ReferenceTargetKind, SymbolKind, document_element_at, document_symbols,
        generate_heading_ids, heading_id_base, reference_targets, render_symbols_json,
        source_language_candidates,
    };
    pub use crate::inline::{inline_at, is_plain_inline_text};
    pub use crate::inline_model::{
        AttributeUse, Inline, InlineFormula, InlineLiteralKind, InlineProblem, InlineProblemKind,
        InlineStyle, InlineText, Link, MacroAttribute, MacroForm, MathLanguage, PassthroughKind,
        Reference, ReferenceDestination, StandardMacro, StandardMacroKind,
    };
    pub use crate::presentation::{
        BibliographySection, BlockId, DocumentIndex, DocumentLayout, DocumentPresentation,
        GeneratedLayoutNode, HeadingPresentation, LayoutNode, LayoutScope, TocPolicy,
    };
    pub use crate::resolved::DocumentFacts;
    pub use crate::structure::{
        DocumentStructure, Manpage, Section, SectionKind, StructureProblem, StructureProblemKind,
        StructuredHeading, TocEntry,
    };
    pub use crate::substitution::{
        AttributeExpansionError, AttributeExpansionLimits, SubstitutionContext, SubstitutionStep,
    };
    pub use crate::table::{
        HorizontalAlignment, Table, TableCell, TableCellContent, TableCellStyle, TableColumn,
        TableFormat, TableFrame, TableGrid, TablePresentation, TableProblem, TableProblemKind,
        TableRow, TableSection, TableStripes, VerticalAlignment,
    };
    pub use crate::walker::{SemanticNode, walk};
}

/// Deterministic document output and serialization backends.
pub mod output {
    pub mod conformance {
        pub use crate::conformance::{
            ConformanceSnapshot, DocumentProducts, ProductSet, fixture_source, products, snapshot,
        };
    }
    pub mod diagnostics {
        pub use crate::diagnostic::{
            Applicability, CoreErrorCode, Diagnostic, DiagnosticCode, DiagnosticId, EditConflict,
            EditConflictKind, Fix, RelatedInformation, Severity, TextEdit, render_human,
            render_json, sort_diagnostics,
        };
        pub use crate::lint::{
            ASCIIDOC_FILE_LINK, ATTRIBUTE_EXPANSION, DUPLICATE_ANCHOR, DUPLICATE_HEADING_ID,
            EXCESSIVE_BLANK_LINES, HEADING_MARKER_SPACE, INCONSISTENT_LIST, INVALID_ANCHOR,
            INVALID_ATTRIBUTE, INVALID_CATALOG, INVALID_CROSS_REFERENCE,
            INVALID_DOCUMENT_STRUCTURE, INVALID_HEADING_LEVEL, INVALID_LIST_PRESENTATION,
            INVALID_STEM, INVALID_TABLE, INVALID_URL_SCHEME, LINE_TOO_LONG, LINT_RULES, LintConfig,
            LintError, LintRuleDescriptor, LintRuleId, MACRO_BOUNDARY, MISSING_SOURCE_LANGUAGE,
            MONOSPACE_BOUNDARY, NESTING_LIMIT_EXCEEDED, NON_ASCIIDOC_XREF, PROTECTED_ATTRIBUTE,
            RuleSettings, TRAILING_WHITESPACE, UNCLOSED_BLOCK, UNCLOSED_INLINE,
            UNDEFINED_ATTRIBUTE, UNPROCESSED_DIRECTIVE, UNRESOLVED_CROSS_REFERENCE,
            UNUSED_ATTRIBUTE, lint_analysis, lint_rule, render_lint_rule_catalog_json,
        };
    }
    pub mod formatter {
        pub use crate::formatter::{
            FormatConfig, FormatError, FormatOutput, NewlineStyle, format_analysis,
        };
    }
    pub mod html {
        pub use crate::html::{
            ALLOWED_ATTRIBUTES, ALLOWED_CLASSES, ALLOWED_ELEMENTS, ExternalLinkPresentation,
            HtmlDocumentMode, HtmlOutput, MathLanguagePolicy, RenderPolicy, ResolvedReference,
            ResourceCapabilities, SourceLanguagePolicy, StylesheetPolicy, StylesheetSource,
            UnknownSourceLanguage, UnresolvedReferencePresentation, render, render_with_inputs,
        };
    }
    pub mod projection {
        pub use crate::projection::{
            BlockPresentationKind, BlockPresentationProjection, DocumentProjection, ExternalLink,
            FormulaKind, FormulaProjection, OrderedListProjection, ProjectedText, ReferenceEdge,
            RenderingFeatures, SearchTextKind, SearchTextSegment, SearchableText,
            SourceBlockProjection, project, searchable_text,
        };
    }
}

/// Deterministic preprocessing over caller-provided resource snapshots.
pub mod preprocess {
    pub use crate::preprocessor::{
        AnalysisProjection, Directive, DirectiveKind, EffectivePreprocessStep,
        EffectiveProcessingOptions, EffectiveSuspendedPreprocess, ExpandedOffset, ExpandedRange,
        HostResourceError, HostResourceErrorKind, IncludeRequest, OriginRange, Originated,
        PreparedAnalysisError, PreparedPreprocessedDocument, PreprocessError, PreprocessErrorKind,
        PreprocessFailure, PreprocessInputs, PreprocessNotice, PreprocessNoticeKind,
        PreprocessOptions, PreprocessStep, PreprocessedAnalysis, PreprocessedAnalysisError,
        PreprocessedDocument, ProcessingOptionsError, ProjectedAttributeBinding,
        ProjectedAttributeReference, ProjectedDiagnostic, ProjectedDocumentAttribute,
        ProjectedDocumentAttributeValueLine, ProjectedDocumentSymbol, ProjectedFix,
        ProjectedLocalTarget, ProjectedReference, ProjectedResource, ProjectionError,
        ProjectionFailure, ProjectionLimits, ResourceDocument, ResourceLookup,
        ResourceLookupResult, ResourceRequest, ResourceResponse, ResourceSnapshot, SafeMode,
        SourceMapSegment, SourceMapping, SourceOrigin, SuspendedPreprocess, discover_includes,
        preprocess, preprocess_and_analyze, preprocess_and_analyze_with, preprocess_resumable,
        preprocess_with, resolve_include_target,
    };
}

/// Host-provided reference, resource and citation resolution contracts.
pub mod resolution {
    pub use crate::citation::{CitationOutcome, CitationSegment, ResolvedCitation};
    pub use crate::generated_bibliography::{GeneratedBibliography, GeneratedBibliographyEntry};
    pub use crate::reference::{
        DocumentCandidate, ReferenceKey, ReferenceQuery, ReferenceResolver, ResolutionCacheKey,
        ResolutionFailureKind, ResolutionNotice, ResolutionNoticeKind, ResolutionOutcome,
        ResolvedReference, ResolverFailure, ResolverFuture, ReverseReference, query_from_reference,
    };
    pub use crate::render::{
        RenderInputDomain, RenderInputProblem, RenderInputProblemKind, RenderInputUsage,
        RenderInputs, ResolutionMatch,
    };
    pub use crate::resource::{
        InvalidMediaType, MediaFamily, MediaType, ResolvedResource, ResourceFailure,
        ResourceFailureKind, ResourceFuture, ResourceOutcome, ResourcePurpose, ResourceQuery,
        ResourceReference, ResourceResolver, ResourceValue,
    };
    pub use crate::url::{ActiveUrlPolicy, AuthoredUrlPolicy, UrlDecision, UrlProvenance};
}

/// Source positions and the lossless syntax tree.
pub mod text {
    pub use crate::source::{
        LineEnding, LosslessToken, LosslessTokenKind, Position, PositionEncoding, PositionError,
        SourceDocument, SourceLine, TextRange, TextSize,
    };
    pub use crate::syntax::{
        SyntaxDescendants, SyntaxFix, SyntaxIssue, SyntaxIssueClass, SyntaxIssueDetail, SyntaxKind,
        SyntaxNode, SyntaxTree,
    };
}

pub use core::{
    Analysis, AnalysisInputs, AnalysisOptions, CancellationCheck, CancellationToken,
    DiagnosticProfile, Engine, NeverCancel, ParseError, SourceId, SyntaxOptions,
};
pub use execution::{AnalysisCacheKey, AnalysisRequest, AnalysisResult, DocumentRevision};
pub use limits::{AnalysisLimits, OutputLimits, SyntaxMode};
pub use local_target::{LocalTargetKind, LocalTargetReference, LocalTargetSyntax};

pub const PRODUCT_NAME: &str = "AdocWeave";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
