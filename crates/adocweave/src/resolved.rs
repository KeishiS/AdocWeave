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
    fn build(
        document: &AstDocument,
        attributes: &AttributeEnvironment,
        checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
    ) -> Result<Self, ()> {
        let mut facts = Self::default();
        for binding in attributes.bindings() {
            if checkpoint.is_cancelled() {
                return Err(());
            }
            for reference in crate::attributes::value_references(binding, attributes) {
                if checkpoint.is_cancelled() {
                    return Err(());
                }
                facts.attribute_references.push(reference);
            }
        }
        let walked = crate::walker::try_walk_ast(document, |node| {
            if checkpoint.is_cancelled() {
                return std::ops::ControlFlow::Break(());
            }
            match node {
                crate::walker::SemanticNode::Inline(
                    crate::inline::Inline::AttributeReference {
                        range,
                        name_range,
                        name,
                        ..
                    },
                ) => facts.attribute_references.push(attribute_reference(
                    name,
                    *range,
                    *name_range,
                    attributes,
                )),
                crate::walker::SemanticNode::Inline(crate::inline::Inline::Link(link)) => {
                    facts.links.push(link.clone());
                    for reference in &link.target_attributes {
                        if checkpoint.is_cancelled() {
                            return std::ops::ControlFlow::Break(());
                        }
                        facts
                            .attribute_references
                            .push(attribute_use(reference, attributes));
                    }
                }
                crate::walker::SemanticNode::Inline(crate::inline::Inline::Reference(
                    reference,
                )) => {
                    facts.references.push(reference.clone());
                    for reference in &reference.target_attributes {
                        if checkpoint.is_cancelled() {
                            return std::ops::ControlFlow::Break(());
                        }
                        facts
                            .attribute_references
                            .push(attribute_use(reference, attributes));
                    }
                }
                crate::walker::SemanticNode::Inline(crate::inline::Inline::Macro(node)) => {
                    facts.macros.push(node.clone());
                    for reference in &node.target_attributes {
                        if checkpoint.is_cancelled() {
                            return std::ops::ControlFlow::Break(());
                        }
                        facts
                            .attribute_references
                            .push(attribute_use(reference, attributes));
                    }
                    for resource in crate::resource::ResourceReference::from_macro(node) {
                        if checkpoint.is_cancelled() {
                            return std::ops::ControlFlow::Break(());
                        }
                        facts.resources.push(resource);
                    }
                }
                _ => {}
            }
            std::ops::ControlFlow::Continue(())
        });
        if walked.is_break() {
            return Err(());
        }
        crate::cancellation::sort_by_cancellable(
            &mut facts.attribute_references,
            &mut |left, right| left.range.start().cmp(&right.range.start()),
            checkpoint,
        )?;
        crate::cancellation::sort_by_cancellable(
            &mut facts.links,
            &mut |left, right| left.range.start().cmp(&right.range.start()),
            checkpoint,
        )?;
        crate::cancellation::sort_by_cancellable(
            &mut facts.references,
            &mut |left, right| left.range.start().cmp(&right.range.start()),
            checkpoint,
        )?;
        crate::cancellation::sort_by_cancellable(
            &mut facts.macros,
            &mut |left, right| left.range.start().cmp(&right.range.start()),
            checkpoint,
        )?;
        crate::cancellation::sort_by_cancellable(
            &mut facts.resources,
            &mut |left, right| left.range().start().cmp(&right.range().start()),
            checkpoint,
        )?;
        Ok(facts)
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResolvedBuildFailure {
    Limit(crate::catalog::CatalogLimitExceeded),
    Cancelled,
}

impl ResolvedDocument {
    pub(crate) fn build(
        document: &AstDocument,
        attributes: AttributeEnvironment,
        catalog_limits: AnalysisLimits,
        checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
    ) -> Result<Self, ResolvedBuildFailure> {
        let facts = DocumentFacts::build(document, &attributes, checkpoint)
            .map_err(|()| ResolvedBuildFailure::Cancelled)?;
        let identifiers = crate::document::build_identifiers(document, checkpoint)
            .map_err(|()| ResolvedBuildFailure::Cancelled)?;
        let structure = crate::structure::build(document, &identifiers, checkpoint)
            .map_err(|()| ResolvedBuildFailure::Cancelled)?;
        let index = crate::presentation::build_index(document, checkpoint)
            .map_err(|()| ResolvedBuildFailure::Cancelled)?;
        let presentation = crate::presentation::build_presentation(
            document,
            &structure,
            &index,
            &attributes,
            checkpoint,
        )
        .map_err(|()| ResolvedBuildFailure::Cancelled)?;
        let layout = crate::presentation::build_layout(document, &index, &presentation, checkpoint)
            .map_err(|()| ResolvedBuildFailure::Cancelled)?;
        let catalogs =
            crate::catalog::build(&facts, &index, catalog_limits, checkpoint).map_err(|error| {
                match error {
                    crate::catalog::CatalogBuildFailure::Limit(error) => {
                        ResolvedBuildFailure::Limit(error)
                    }
                    crate::catalog::CatalogBuildFailure::Cancelled => {
                        ResolvedBuildFailure::Cancelled
                    }
                }
            })?;
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::DocumentFacts;

    #[test]
    fn document_facts_build_cancels_during_the_semantic_walk() {
        struct CancelAfterFirstCheckpoint(AtomicUsize);

        impl crate::core::CancellationCheck for CancelAfterFirstCheckpoint {
            fn is_cancelled(&self) -> bool {
                self.0.fetch_add(1, Ordering::Relaxed) >= 1
            }
        }

        let source = (0..crate::cancellation::CHECKPOINT_INTERVAL * 2)
            .map(|index| format!("https://example.com/{index}[link]\n\n"))
            .collect::<String>();
        let parsed = crate::parser::parse(&source).expect("parse");
        let cancellation = CancelAfterFirstCheckpoint(AtomicUsize::new(0));

        let result = DocumentFacts::build(
            &parsed.ast,
            parsed.ast.attribute_environment(),
            &mut crate::cancellation::CancellationCheckpoint::new(&cancellation),
        );

        assert_eq!(result, Err(()));
        assert_eq!(cancellation.0.load(Ordering::Relaxed), 2);
    }
}
