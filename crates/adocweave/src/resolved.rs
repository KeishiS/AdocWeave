//! Construction and ownership of immutable document-wide semantic facts.
//!
//! The raw semantic tree is complete before this model is built. Dependencies
//! between derived views are passed explicitly so no consumer can observe a
//! partially resolved document.

use crate::limits::ProcessingLimits;
use crate::parser::AstDocument;
use crate::presentation::ResolvedDocumentAttributes;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ResolvedDocument {
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
        let identifiers = crate::document::build_identifiers(document);
        let structure = crate::structure::build(document, &identifiers);
        let index = crate::presentation::build_index(document);
        let presentation =
            crate::presentation::build_presentation(document, &structure, &index, attributes);
        let layout = crate::presentation::build_layout(document, &index, &presentation);
        let catalogs = crate::catalog::build(document, &index, catalog_limits)?;
        Ok(Self {
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
