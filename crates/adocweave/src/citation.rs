//! Citations of entries held by a bibliography library outside the document.
//!
//! A `cite:` macro names keys that AdocWeave never resolves. The host owns the
//! library, so this module only records what the source states: which keys were
//! cited, where each one sits, and what the author attached to the citation.

use crate::inline::{MacroAttribute, StandardMacro, StandardMacroKind};
use crate::source::TextRange;

/// One `cite:` macro with every key it names.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Citation {
    /// Range of the whole macro, including `cite:` and the brackets.
    pub range: TextRange,
    /// Position of this citation among the citations of the document, from 0.
    pub order: u32,
    /// Cited keys in source order. A citation always names at least one key.
    pub keys: Vec<CitationKey>,
    /// Named attributes such as `locator` that describe the citation itself.
    pub attributes: Vec<MacroAttribute>,
}

/// One key named by a citation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CitationKey {
    /// Range of the key inside the macro, without surrounding whitespace.
    pub range: TextRange,
    pub value: String,
}

/// Collects citations from the standard macros of one analysis.
///
/// Positional attributes are keys in source order. Named attributes describe
/// the citation and never become keys, so `cite:[a, locator="p. 12"]` names one
/// key while `cite:[a, b]` names two.
pub(crate) fn citations(macros: &[StandardMacro]) -> Vec<Citation> {
    let mut order = 0;
    macros
        .iter()
        .filter(|node| node.kind == StandardMacroKind::Citation)
        .map(|node| {
            let keys = node
                .attributes
                .iter()
                .filter(|attribute| attribute.name.is_none())
                .map(|attribute| CitationKey {
                    range: attribute.value_range,
                    value: attribute.value.clone(),
                })
                .collect();
            let attributes = node
                .attributes
                .iter()
                .filter(|attribute| attribute.name.is_some())
                .cloned()
                .collect();
            let citation = Citation {
                range: node.range,
                order,
                keys,
                attributes,
            };
            order += 1;
            citation
        })
        .collect()
}

/// One `cite:` macro as the host's bibliography library resolved it.
///
/// AdocWeave never reads the library, so the host owns both the citation text
/// and the decision of which part of it links where. The resolution is matched
/// to the macro by its exact source range, like every other render input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedCitation {
    /// Range of the whole `cite:` macro, as reported by [`Citation::range`].
    pub source_range: TextRange,
    pub outcome: CitationOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CitationOutcome {
    /// Citation text in reading order, split so parts of it can link.
    Resolved {
        segments: Vec<CitationSegment>,
    },
    Failed(crate::reference::ResolverFailure),
}

/// One run of citation text, optionally linking to a bibliography entry.
///
/// A citation style decides its own punctuation, so the host sends the text it
/// wants shown. Splitting it into segments lets `(Smith 2024; Tanaka 2025)`
/// link each name while leaving the brackets and separator as plain text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CitationSegment {
    pub text: String,
    /// Anchor of the entry this run names, without `#`. `None` leaves it plain.
    pub anchor: Option<String>,
}

impl CitationSegment {
    /// Builds a run of plain citation text.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            anchor: None,
        }
    }

    /// Builds a run of citation text linking to an entry in the same document.
    pub fn linked(text: impl Into<String>, anchor: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            anchor: Some(anchor.into()),
        }
    }
}

impl ResolvedCitation {
    pub fn resolved(source_range: TextRange, segments: Vec<CitationSegment>) -> Self {
        Self {
            source_range,
            outcome: CitationOutcome::Resolved { segments },
        }
    }

    pub fn failed(source_range: TextRange, failure: crate::reference::ResolverFailure) -> Self {
        Self {
            source_range,
            outcome: CitationOutcome::Failed(failure),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{AnalysisOptions, Engine};

    #[test]
    fn a_citation_without_a_key_is_diagnosed_instead_of_disappearing() {
        for source in ["Empty cite:[] here.\n", "Blank cite:[ ] here.\n"] {
            let analysis = Engine::new(AnalysisOptions::default())
                .analyze(source)
                .expect("analysis");
            let diagnostics = analysis.diagnostics();
            let citation = diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.message == "citation names no bibliography key")
                .collect::<Vec<_>>();
            assert_eq!(citation.len(), 1, "{source}");
            assert_eq!(citation[0].code.as_str(), "invalid-catalog");
            assert_eq!(
                &source[citation[0].range.start().to_usize()..][..5],
                "cite:",
                "{source}"
            );
        }

        let valid = Engine::new(AnalysisOptions::default())
            .analyze("See cite:[smith2024].\n")
            .expect("analysis");
        assert!(
            !valid
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.message == "citation names no bibliography key")
        );
    }

    #[test]
    fn citations_separate_keys_from_named_attributes_and_keep_source_order() {
        let source = "See cite:[smith2024, tanaka2025] then cite:[a, locator=\"p. 12\"].\n";
        let analysis = Engine::new(AnalysisOptions::default())
            .analyze(source)
            .expect("analysis");
        let citations = analysis.citations();

        assert_eq!(citations.len(), 2);
        assert_eq!(citations[0].order, 0);
        assert_eq!(
            citations[0]
                .keys
                .iter()
                .map(|key| key.value.as_str())
                .collect::<Vec<_>>(),
            ["smith2024", "tanaka2025"]
        );
        assert!(citations[0].attributes.is_empty());

        assert_eq!(citations[1].order, 1);
        assert_eq!(
            citations[1]
                .keys
                .iter()
                .map(|key| key.value.as_str())
                .collect::<Vec<_>>(),
            ["a"]
        );
        assert_eq!(
            citations[1]
                .attributes
                .iter()
                .filter_map(|attribute| attribute.name.as_deref())
                .collect::<Vec<_>>(),
            ["locator"]
        );

        // Every recorded range addresses the original source.
        for citation in &citations {
            assert_eq!(&source[citation.range.start().to_usize()..][..5], "cite:");
            for key in &citation.keys {
                assert_eq!(
                    &source[key.range.start().to_usize()..key.range.end().to_usize()],
                    key.value
                );
            }
        }
    }
}
