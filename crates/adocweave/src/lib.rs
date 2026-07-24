//! Core application boundary for AdocWeave.
//!
//! The command-line interface is a host adapter around this API and owns file
//! and standard-stream I/O. Parsing, diagnostics, formatting, and rendering
//! remain deterministic core operations over caller-provided input.

mod attributes;
mod block_model;
mod block_grammar;
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
    pub use crate::attributes::{DocumentAttributeOccurrence, DocumentAttributeOperation};
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
    pub use crate::catalog::*;
    pub use crate::document::*;
    pub use crate::inline::*;
    pub use crate::presentation::*;
    pub use crate::resolved::DocumentFacts;
    pub use crate::structure::*;
    pub use crate::substitution::*;
    pub use crate::table::*;
    pub use crate::walker::*;
}

/// Deterministic document output and serialization backends.
pub mod output {
    pub mod conformance {
        pub use crate::conformance::*;
    }
    pub mod diagnostics {
        pub use crate::diagnostic::*;
        pub use crate::lint::*;
    }
    pub mod formatter {
        pub use crate::formatter::*;
    }
    pub mod html {
        pub use crate::html::*;
    }
    pub mod projection {
        pub use crate::projection::*;
    }
}

/// Deterministic preprocessing over caller-provided resource snapshots.
pub mod preprocess {
    pub use crate::preprocessor::*;
}

/// Host-provided reference and resource resolution contracts.
pub mod resolution {
    pub use crate::reference::*;
    pub use crate::render::*;
    pub use crate::resource::*;
    pub use crate::url::*;
}

/// Source positions and the lossless syntax tree.
pub mod text {
    pub use crate::source::*;
    pub use crate::syntax::*;
}

pub use conformance::{DocumentProducts, ProductSet};
pub use core::{
    Analysis, CancellationCheck, CancellationToken, Engine, NeverCancel, ParseError, ParseOptions,
    SourceId,
};
pub use execution::{AnalysisRequest, AnalysisResult, DocumentRevision};
pub use limits::{ProcessingLimits, SyntaxMode};

pub const PRODUCT_NAME: &str = "AdocWeave";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
