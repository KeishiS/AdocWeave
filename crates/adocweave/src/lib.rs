//! Core application boundary for AdocWeave.
//!
//! The command-line interface is a host adapter around this API and owns file
//! and standard-stream I/O. Parsing, diagnostics, formatting, and rendering
//! remain deterministic core operations over caller-provided input.

mod attributes;
mod block_grammar;
mod block_model;
mod block_sequence;
mod budget;
mod catalog;
mod conformance;
mod core;
mod delimiter;
mod diagnostic;
mod document;
mod document_header;
mod execution;
mod formatter;
mod html;
mod inline;
mod inline_grammar;
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
mod source_document;
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
        VerbatimBlock, VerbatimKind,
    };
    pub use crate::catalog::{
        BibliographyEntry, BibliographyReference, CatalogProblem, CatalogProblemKind,
        DocumentCatalogs, Footnote, FootnoteOccurrence, IndexEntry,
    };
    pub use crate::document::{
        Document, DocumentElement, DocumentIdentifiers, DocumentSymbol, HeadingId, ReferenceTarget,
        ReferenceTargetKind, SymbolKind, document_element_at, document_symbols,
        generate_heading_ids, heading_id_base, reference_targets, render_symbols_json,
        source_language_candidates,
    };
    pub use crate::inline::{
        AttributeUse, Inline, InlineFormula, InlineLiteralKind, InlineProblem, InlineProblemKind,
        InlineStyle, InlineText, Link, MacroAttribute, MacroForm, MathLanguage, PassthroughKind,
        Reference, ReferenceDestination, StandardMacro, StandardMacroKind, inline_at,
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
            LintRuleDescriptor, LintRuleId, MACRO_BOUNDARY, MISSING_SOURCE_LANGUAGE,
            NESTING_LIMIT_EXCEEDED, NON_ASCIIDOC_XREF, PROTECTED_ATTRIBUTE, RuleSettings,
            TRAILING_WHITESPACE, UNCLOSED_BLOCK, UNCLOSED_INLINE, UNDEFINED_ATTRIBUTE,
            UNRESOLVED_CROSS_REFERENCE, UNUSED_ATTRIBUTE, lint_analysis, lint_rule,
            render_lint_rule_catalog_json,
        };
    }
    pub mod formatter {
        pub use crate::formatter::{FormatConfig, FormatOutput, NewlineStyle, format_analysis};
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
            SearchTextKind, SearchTextSegment, SearchableText, SourceBlockProjection, project,
            searchable_text,
        };
    }
}

/// Deterministic preprocessing over caller-provided resource snapshots.
pub mod preprocess {
    pub use crate::preprocessor::{
        AnalysisProjection, Directive, DirectiveKind, ExpandedOffset, ExpandedRange,
        IncludeRequest, OriginRange, Originated, PreprocessError, PreprocessErrorKind,
        PreprocessNotice, PreprocessNoticeKind, PreprocessOptions, PreprocessedAnalysis,
        PreprocessedAnalysisError, PreprocessedDocument, ProjectedDiagnostic,
        ProjectedDocumentAttribute, ProjectedDocumentAttributeValueLine, ProjectedDocumentSymbol,
        ProjectedFix, ProjectedLocalTarget, ProjectedReference, ProjectedResource, ProjectionError,
        ProjectionLimits, ResourceDocument, ResourceSnapshot, SafeMode, SourceMapSegment,
        SourceMapping, SourceOrigin, discover_includes, preprocess, preprocess_and_analyze,
        resolve_include_target,
    };
}

/// Host-provided reference and resource resolution contracts.
pub mod resolution {
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

pub use conformance::{DocumentProducts, ProductSet};
pub use core::{
    Analysis, AnalysisOptions, CancellationCheck, CancellationToken, DiagnosticProfile, Engine,
    NeverCancel, ParseError, SourceId, SyntaxOptions,
};
pub use execution::{AnalysisRequest, AnalysisResult, DocumentRevision};
pub use limits::{AnalysisLimits, OutputLimits, SyntaxMode};
pub use local_target::{LocalTargetKind, LocalTargetReference, LocalTargetSyntax};

pub const PRODUCT_NAME: &str = "AdocWeave";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
