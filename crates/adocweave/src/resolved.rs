//! Construction and ownership of immutable document-wide semantic facts.
//!
//! The raw semantic tree is complete before this model is built. Dependencies
//! between derived views are passed explicitly so no consumer can observe a
//! partially resolved document.

use crate::attributes::AttributeEnvironment;
use crate::limits::AnalysisLimits;
use crate::parser::AstDocument;

/// Immutable, source-ordered facts collected from a semantic document in one pass.
///
/// Facts are independent of output backends and host resolution. Derived views
/// such as catalogs, reference queries, and resource queries consume this
/// index instead of traversing the document tree again.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DocumentFacts {
    attribute_references: Vec<crate::attributes::AttributeReference>,
    links: Vec<crate::inline::Link>,
    references: Vec<crate::inline::Reference>,
    macros: Vec<crate::inline::StandardMacro>,
    resources: Vec<crate::resource::ResourceReference>,
}

impl DocumentFacts {
    fn build(document: &AstDocument, attributes: &AttributeEnvironment) -> Self {
        let mut facts = Self::default();
        for binding in attributes.bindings() {
            facts
                .attribute_references
                .extend(crate::attributes::value_references(binding, attributes));
        }
        crate::walker::walk_ast(document, |node| match node {
            crate::walker::SemanticNode::Inline(crate::inline::Inline::AttributeReference {
                range,
                name_range,
                name,
                ..
            }) => facts.attribute_references.push(attribute_reference(
                name,
                *range,
                *name_range,
                attributes,
            )),
            crate::walker::SemanticNode::Inline(crate::inline::Inline::Link(link)) => {
                facts.links.push(link.clone());
                facts.attribute_references.extend(
                    link.target_attributes
                        .iter()
                        .map(|reference| attribute_use(reference, attributes)),
                );
            }
            crate::walker::SemanticNode::Inline(crate::inline::Inline::Reference(reference)) => {
                facts.references.push(reference.clone());
                facts.attribute_references.extend(
                    reference
                        .target_attributes
                        .iter()
                        .map(|reference| attribute_use(reference, attributes)),
                );
            }
            crate::walker::SemanticNode::Inline(crate::inline::Inline::Macro(node)) => {
                facts.macros.push(node.clone());
                facts.attribute_references.extend(
                    node.target_attributes
                        .iter()
                        .map(|reference| attribute_use(reference, attributes)),
                );
                facts
                    .resources
                    .extend(crate::resource::ResourceReference::from_macro(node));
            }
            _ => {}
        });
        facts
            .attribute_references
            .sort_by_key(|reference| reference.range.start());
        facts.links.sort_by_key(|link| link.range.start());
        facts
            .references
            .sort_by_key(|reference| reference.range.start());
        facts.macros.sort_by_key(|node| node.range.start());
        facts
            .resources
            .sort_by_key(|resource| resource.range().start());
        facts
    }

    pub fn attribute_references(&self) -> &[crate::attributes::AttributeReference] {
        &self.attribute_references
    }

    pub fn links(&self) -> &[crate::inline::Link] {
        &self.links
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

fn attribute_reference(
    name: &str,
    range: crate::source::TextRange,
    name_range: crate::source::TextRange,
    attributes: &AttributeEnvironment,
) -> crate::attributes::AttributeReference {
    crate::attributes::reference_at(
        name,
        range,
        name_range,
        crate::attributes::AttributePosition::new(
            name_range.start(),
            crate::attributes::AttributeEventId::new(u32::MAX),
        ),
        attributes,
    )
}

fn attribute_use(
    reference: &crate::inline::AttributeUse,
    attributes: &AttributeEnvironment,
) -> crate::attributes::AttributeReference {
    let start = reference
        .name_range
        .start()
        .to_u32()
        .checked_sub(1)
        .and_then(|value| crate::source::TextSize::new(value as usize).ok())
        .unwrap_or(reference.name_range.start());
    let end = reference
        .name_range
        .end()
        .to_u32()
        .checked_add(1)
        .and_then(|value| crate::source::TextSize::new(value as usize).ok())
        .unwrap_or(reference.name_range.end());
    let range = crate::source::TextRange::new(start, end).unwrap_or(reference.name_range);
    attribute_reference(&reference.name, range, reference.name_range, attributes)
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ResolvedDocument {
    attribute_environment: AttributeEnvironment,
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
        attributes: AttributeEnvironment,
        catalog_limits: AnalysisLimits,
    ) -> Result<Self, crate::catalog::CatalogLimitExceeded> {
        let facts = DocumentFacts::build(document, &attributes);
        let identifiers = crate::document::build_identifiers(document);
        let structure = crate::structure::build(document, &identifiers);
        let index = crate::presentation::build_index(document);
        let presentation =
            crate::presentation::build_presentation(document, &structure, &index, &attributes);
        let layout = crate::presentation::build_layout(document, &index, &presentation);
        let catalogs = crate::catalog::build(&facts, &index, catalog_limits)?;
        Ok(Self {
            attribute_environment: attributes,
            facts,
            catalogs,
            identifiers,
            structure,
            index,
            presentation,
            layout,
        })
    }

    pub(crate) const fn attribute_environment(&self) -> &AttributeEnvironment {
        &self.attribute_environment
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
