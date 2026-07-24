//! Construction and ownership of immutable document-wide semantic facts.
//!
//! The raw semantic tree is complete before this model is built. Dependencies
//! between derived views are passed explicitly so no consumer can observe a
//! partially resolved document.

use crate::limits::ProcessingLimits;
use crate::parser::AstDocument;
use crate::presentation::ResolvedDocumentAttributes;

/// Immutable, source-ordered facts collected from a semantic document in one pass.
///
/// Facts are independent of output backends and host resolution. Derived views
/// such as catalogs, reference queries, and resource queries consume this
/// index instead of traversing the document tree again.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DocumentFacts {
    references: Vec<crate::inline::Reference>,
    macros: Vec<crate::inline::StandardMacro>,
    resources: Vec<crate::resource::ResourceReference>,
}

impl DocumentFacts {
    fn build(document: &AstDocument) -> Self {
        let mut facts = Self::default();
        crate::walker::walk_ast(document, |node| match node {
            crate::walker::SemanticNode::Inline(crate::inline::Inline::Reference(reference)) => {
                facts.references.push(reference.clone());
            }
            crate::walker::SemanticNode::Inline(crate::inline::Inline::Macro(node)) => {
                facts.macros.push(node.clone());
                if let Some(resource) = crate::resource::ResourceReference::from_macro(node) {
                    facts.resources.push(resource);
                }
            }
            _ => {}
        });
        facts
            .references
            .sort_by_key(|reference| reference.range.start());
        facts.macros.sort_by_key(|node| node.range.start());
        facts
            .resources
            .sort_by_key(|resource| resource.range.start());
        facts
    }

    pub fn references(&self) -> &[crate::inline::Reference] {
        &self.references
    }

    pub fn macros(&self) -> &[crate::inline::StandardMacro] {
        &self.macros
    }

    pub fn resources(&self) -> &[crate::resource::ResourceReference] {
        &self.resources
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ResolvedDocument {
    facts: DocumentFacts,
    catalogs: crate::catalog::DocumentCatalogs,
    identifiers: crate::document::DocumentIdentifiers,
    structure: crate::structure::DocumentStructure,
    index: crate::presentation::DocumentIndex,
    presentation: crate::presentation::DocumentPresentation,
    layout: crate::presentation::DocumentLayout,
}

impl ResolvedDocument {
    pub(crate) fn build(
        document: &AstDocument,
        attributes: ResolvedDocumentAttributes,
        catalog_limits: ProcessingLimits,
    ) -> Result<Self, crate::catalog::CatalogLimitExceeded> {
        let facts = DocumentFacts::build(document);
        let identifiers = crate::document::build_identifiers(document);
        let structure = crate::structure::build(document, &identifiers);
        let index = crate::presentation::build_index(document);
        let presentation =
            crate::presentation::build_presentation(document, &structure, &index, attributes);
        let layout = crate::presentation::build_layout(document, &index, &presentation);
        let catalogs = crate::catalog::build(&facts, &index, catalog_limits)?;
        Ok(Self {
            facts,
            catalogs,
            identifiers,
            structure,
            index,
            presentation,
            layout,
        })
    }

    pub(crate) const fn catalogs(&self) -> &crate::catalog::DocumentCatalogs {
        &self.catalogs
    }

    pub(crate) const fn facts(&self) -> &DocumentFacts {
        &self.facts
    }

    pub(crate) const fn identifiers(&self) -> &crate::document::DocumentIdentifiers {
        &self.identifiers
    }

    pub(crate) const fn structure(&self) -> &crate::structure::DocumentStructure {
        &self.structure
    }

    pub(crate) const fn index(&self) -> &crate::presentation::DocumentIndex {
        &self.index
    }

    pub(crate) const fn presentation(&self) -> &crate::presentation::DocumentPresentation {
        &self.presentation
    }

    pub(crate) const fn layout(&self) -> &crate::presentation::DocumentLayout {
        &self.layout
    }
}
