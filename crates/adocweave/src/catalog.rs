//! Immutable, backend-independent catalogs derived once from the semantic tree.

use std::collections::BTreeMap;

use crate::inline::{StandardMacro, StandardMacroKind};
use crate::limits::AnalysisLimits;
use crate::reference::ReferenceKey;
use crate::source::TextRange;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DocumentCatalogs {
    footnotes: Vec<Footnote>,
    bibliography: Vec<BibliographyEntry>,
    index: Vec<IndexEntry>,
    problems: Vec<CatalogProblem>,
}

impl DocumentCatalogs {
    pub fn footnotes(&self) -> &[Footnote] {
        &self.footnotes
    }

    pub fn bibliography(&self) -> &[BibliographyEntry] {
        &self.bibliography
    }

    pub fn index(&self) -> &[IndexEntry] {
        &self.index
    }

    pub fn problems(&self) -> &[CatalogProblem] {
        &self.problems
    }

    pub fn footnote_occurrence(&self, range: TextRange) -> Option<(&Footnote, usize)> {
        self.footnotes.iter().find_map(|footnote| {
            footnote
                .occurrences
                .iter()
                .position(|occurrence| occurrence.range == range)
                .map(|index| (footnote, index))
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Footnote {
    pub number: u32,
    pub id: Option<String>,
    pub definition_range: TextRange,
    pub content_range: TextRange,
    pub text: String,
    pub occurrences: Vec<FootnoteOccurrence>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FootnoteOccurrence {
    pub range: TextRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BibliographyEntry {
    pub id: String,
    pub definition_range: TextRange,
    pub definition_block: crate::presentation::BlockId,
    pub references: Vec<BibliographyReference>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BibliographyReference {
    pub range: TextRange,
    pub block: crate::presentation::BlockId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexEntry {
    pub terms: Vec<String>,
    pub display: String,
    pub occurrences: Vec<TextRange>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogProblemKind {
    MissingFootnoteDefinition,
    DuplicateFootnoteDefinition,
    DuplicateBibliographyEntry,
    EmptyIndexTerm,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CatalogProblem {
    pub kind: CatalogProblemKind,
    pub range: TextRange,
    pub related_range: Option<TextRange>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CatalogLimitExceeded {
    pub resource: &'static str,
    pub limit: u32,
    pub actual: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CatalogBuildFailure {
    Limit(CatalogLimitExceeded),
    Cancelled,
}

pub(crate) fn build(
    facts: &crate::resolved::DocumentFacts,
    document_index: &crate::presentation::DocumentIndex,
    limits: AnalysisLimits,
    checkpoint: &mut crate::cancellation::CancellationCheckpoint<'_>,
) -> Result<DocumentCatalogs, CatalogBuildFailure> {
    let mut catalogs = DocumentCatalogs::default();
    let mut named_footnotes = BTreeMap::<String, usize>::new();
    let mut pending_references = Vec::<(String, TextRange)>::new();
    let mut bibliography_references =
        Vec::<(String, TextRange, crate::presentation::BlockId)>::new();
    let mut bibliography = BTreeMap::<String, usize>::new();
    let mut index = BTreeMap::<Vec<String>, usize>::new();
    let mut catalog_bytes = 0_u64;

    for node in facts.macros() {
        if checkpoint.is_cancelled() {
            return Err(CatalogBuildFailure::Cancelled);
        }
        match node {
            node if node.kind == StandardMacroKind::Footnote => {
                let text = node
                    .attributes
                    .first()
                    .map(|attribute| attribute.value.as_str());
                if node.target.is_empty() {
                    if let Some(text) = text.filter(|text| !text.is_empty()) {
                        push_footnote(&mut catalogs, node, None, text, &mut catalog_bytes);
                    }
                } else if let Some(text) = text.filter(|text| !text.is_empty()) {
                    if let Some(existing) = named_footnotes.get(&node.target).copied() {
                        catalogs.problems.push(CatalogProblem {
                            kind: CatalogProblemKind::DuplicateFootnoteDefinition,
                            range: node.range,
                            related_range: Some(catalogs.footnotes[existing].definition_range),
                        });
                        catalogs.footnotes[existing]
                            .occurrences
                            .push(FootnoteOccurrence { range: node.range });
                    } else {
                        let index = catalogs.footnotes.len();
                        push_footnote(
                            &mut catalogs,
                            node,
                            Some(node.target.clone()),
                            text,
                            &mut catalog_bytes,
                        );
                        named_footnotes.insert(node.target.clone(), index);
                    }
                } else if let Some(existing) = named_footnotes.get(&node.target).copied() {
                    catalogs.footnotes[existing]
                        .occurrences
                        .push(FootnoteOccurrence { range: node.range });
                } else {
                    pending_references.push((node.target.clone(), node.range));
                }
            }
            node if node.kind == StandardMacroKind::BibliographyAnchor => {
                if let Some(existing) = bibliography.get(&node.target).copied() {
                    catalogs.problems.push(CatalogProblem {
                        kind: CatalogProblemKind::DuplicateBibliographyEntry,
                        range: node.range,
                        related_range: Some(catalogs.bibliography[existing].definition_range),
                    });
                } else if !node.target.is_empty() {
                    bibliography.insert(node.target.clone(), catalogs.bibliography.len());
                    catalog_bytes += node.target.len() as u64;
                    catalogs.bibliography.push(BibliographyEntry {
                        id: node.target.clone(),
                        definition_range: node.range,
                        definition_block: document_index
                            .block_containing(node.range)
                            .expect("bibliography anchor belongs to a semantic block"),
                        references: Vec::new(),
                    });
                }
            }
            node if node.kind == StandardMacroKind::IndexTerm => {
                let mut terms = Vec::new();
                for attribute in &node.attributes {
                    if checkpoint.is_cancelled() {
                        return Err(CatalogBuildFailure::Cancelled);
                    }
                    let term = attribute.value.trim().to_owned();
                    if !term.is_empty() {
                        terms.push(term);
                    }
                }
                if terms.is_empty() {
                    catalogs.problems.push(CatalogProblem {
                        kind: CatalogProblemKind::EmptyIndexTerm,
                        range: node.range,
                        related_range: None,
                    });
                } else if let Some(existing) = index.get(&terms).copied() {
                    catalogs.index[existing].occurrences.push(node.range);
                } else {
                    let display = terms.join(", ");
                    for term in &terms {
                        if checkpoint.is_cancelled() {
                            return Err(CatalogBuildFailure::Cancelled);
                        }
                        catalog_bytes = catalog_bytes.saturating_add(term.len() as u64);
                    }
                    index.insert(terms.clone(), catalogs.index.len());
                    catalogs.index.push(IndexEntry {
                        terms,
                        display,
                        occurrences: vec![node.range],
                    });
                }
            }
            _ => {}
        }
    }
    for reference in facts.references() {
        if checkpoint.is_cancelled() {
            return Err(CatalogBuildFailure::Cancelled);
        }
        let Some(ReferenceKey::Local { anchor }) = &reference.target else {
            continue;
        };
        bibliography_references.push((
            anchor.clone(),
            reference.range,
            document_index
                .block_containing(reference.range)
                .expect("bibliography reference belongs to a semantic block"),
        ));
    }

    for (id, range) in pending_references {
        if checkpoint.is_cancelled() {
            return Err(CatalogBuildFailure::Cancelled);
        }
        if let Some(existing) = named_footnotes.get(&id).copied() {
            catalogs.footnotes[existing]
                .occurrences
                .push(FootnoteOccurrence { range });
        } else {
            catalogs.problems.push(CatalogProblem {
                kind: CatalogProblemKind::MissingFootnoteDefinition,
                range,
                related_range: None,
            });
        }
    }
    for (id, range, block) in bibliography_references {
        if checkpoint.is_cancelled() {
            return Err(CatalogBuildFailure::Cancelled);
        }
        if let Some(entry) = bibliography
            .get(&id)
            .and_then(|index| catalogs.bibliography.get_mut(*index))
        {
            entry
                .references
                .push(BibliographyReference { range, block });
        }
    }
    for footnote in &mut catalogs.footnotes {
        crate::cancellation::sort_by_cancellable(
            &mut footnote.occurrences,
            &mut |left, right| left.range.start().cmp(&right.range.start()),
            checkpoint,
        )
        .map_err(|()| CatalogBuildFailure::Cancelled)?;
    }
    crate::cancellation::sort_by_cancellable(
        &mut catalogs.footnotes,
        &mut |left, right| {
            let left_start = left
                .occurrences
                .first()
                .map_or(left.definition_range.start(), |item| item.range.start());
            let right_start = right
                .occurrences
                .first()
                .map_or(right.definition_range.start(), |item| item.range.start());
            left_start.cmp(&right_start)
        },
        checkpoint,
    )
    .map_err(|()| CatalogBuildFailure::Cancelled)?;
    for (index, footnote) in catalogs.footnotes.iter_mut().enumerate() {
        if checkpoint.is_cancelled() {
            return Err(CatalogBuildFailure::Cancelled);
        }
        footnote.number = index as u32 + 1;
    }
    for entry in &mut catalogs.bibliography {
        crate::cancellation::sort_by_cancellable(
            &mut entry.references,
            &mut |left, right| left.range.start().cmp(&right.range.start()),
            checkpoint,
        )
        .map_err(|()| CatalogBuildFailure::Cancelled)?;
    }
    crate::cancellation::sort_by_cancellable(
        &mut catalogs.index,
        &mut |left, right| left.terms.cmp(&right.terms),
        checkpoint,
    )
    .map_err(|()| CatalogBuildFailure::Cancelled)?;

    let mut occurrence_count = 0_usize;
    for entry in &catalogs.footnotes {
        if checkpoint.is_cancelled() {
            return Err(CatalogBuildFailure::Cancelled);
        }
        occurrence_count = occurrence_count.saturating_add(entry.occurrences.len());
    }
    for entry in &catalogs.bibliography {
        if checkpoint.is_cancelled() {
            return Err(CatalogBuildFailure::Cancelled);
        }
        occurrence_count = occurrence_count.saturating_add(entry.references.len() + 1);
    }
    for entry in &catalogs.index {
        if checkpoint.is_cancelled() {
            return Err(CatalogBuildFailure::Cancelled);
        }
        occurrence_count = occurrence_count.saturating_add(entry.occurrences.len());
    }
    let entry_count = catalogs.footnotes.len()
        + catalogs.bibliography.len()
        + catalogs.index.len()
        + catalogs.problems.len()
        + occurrence_count;
    if entry_count as u64 > u64::from(limits.max_catalog_entries) {
        return Err(CatalogBuildFailure::Limit(CatalogLimitExceeded {
            resource: "catalog entries",
            limit: limits.max_catalog_entries,
            actual: entry_count as u64,
        }));
    }
    catalog_bytes = catalog_bytes.saturating_add((occurrence_count as u64).saturating_mul(8));
    if catalog_bytes > u64::from(limits.max_catalog_bytes) {
        return Err(CatalogBuildFailure::Limit(CatalogLimitExceeded {
            resource: "catalog bytes",
            limit: limits.max_catalog_bytes,
            actual: catalog_bytes,
        }));
    }
    Ok(catalogs)
}

fn push_footnote(
    catalogs: &mut DocumentCatalogs,
    node: &StandardMacro,
    id: Option<String>,
    text: &str,
    catalog_bytes: &mut u64,
) {
    *catalog_bytes += text.len() as u64 + id.as_ref().map_or(0, String::len) as u64;
    catalogs.footnotes.push(Footnote {
        number: catalogs.footnotes.len() as u32 + 1,
        id,
        definition_range: node.range,
        content_range: node
            .attributes
            .first()
            .map_or(node.target_range, |attribute| attribute.range),
        text: text.to_owned(),
        occurrences: vec![FootnoteOccurrence { range: node.range }],
    });
}

#[cfg(test)]
mod tests {
    use crate::{AnalysisOptions, Engine, ParseError};

    #[test]
    fn catalogs_number_reuse_and_sort_document_wide_facts_once() {
        let source = concat!(
            "footnote:n[] footnote:[anonymous] footnote:n[named] footnote:n[]\n\n",
            "<<ref>> bibanchor:ref[] indexterm:[Rust,Ownership] indexterm:[Rust,Ownership]",
        );
        let analysis = Engine::new(AnalysisOptions::default())
            .analyze(source)
            .expect("analyze");
        let catalogs = analysis.ast().catalogs();
        assert_eq!(catalogs.footnotes().len(), 2);
        assert_eq!(catalogs.footnotes()[0].number, 1);
        assert_eq!(catalogs.footnotes()[0].id.as_deref(), Some("n"));
        assert_eq!(catalogs.footnotes()[0].occurrences.len(), 3);
        assert_eq!(catalogs.footnotes()[1].text, "anonymous");
        assert_eq!(catalogs.bibliography()[0].id, "ref");
        assert_eq!(catalogs.bibliography()[0].references.len(), 1);
        assert_eq!(
            catalogs.bibliography()[0].definition_block,
            catalogs.bibliography()[0].references[0].block
        );
        assert_eq!(catalogs.index()[0].terms, ["Rust", "Ownership"]);
        assert_eq!(catalogs.index()[0].occurrences.len(), 2);
    }

    #[test]
    fn catalogs_retain_missing_and_duplicate_source_ranges() {
        let analysis = Engine::new(AnalysisOptions::default())
            .analyze("footnote:missing[] footnote:n[one] footnote:n[two] bibanchor:b[] bibanchor:b[] indexterm:[]")
            .expect("analyze");
        let problems = analysis.ast().catalogs().problems();
        assert_eq!(problems.len(), 4);
        assert!(problems.iter().any(|problem| {
            problem.kind == super::CatalogProblemKind::DuplicateFootnoteDefinition
                && problem.related_range.is_some()
        }));
    }

    #[test]
    fn catalog_limits_include_duplicate_occurrences_and_output_text() {
        let mut options = AnalysisOptions::default();
        options.syntax.limits.max_catalog_entries = 2;
        assert!(matches!(
            Engine::new(options.clone()).analyze("indexterm:[one] indexterm:[one] indexterm:[one]"),
            Err(ParseError::LimitExceeded {
                resource: "catalog entries",
                ..
            })
        ));
        options.syntax.limits.max_catalog_entries = 100;
        options.syntax.limits.max_catalog_bytes = 4;
        assert!(matches!(
            Engine::new(options).analyze("footnote:[long text]"),
            Err(ParseError::LimitExceeded {
                resource: "catalog bytes",
                ..
            })
        ));
    }
}
