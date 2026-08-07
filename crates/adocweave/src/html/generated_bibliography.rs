//! Validation, usage planning and serialization for host-generated bibliographies.

use std::collections::BTreeMap;

use crate::diagnostic::Diagnostic;
use crate::generated_bibliography::{GeneratedBibliography, GeneratedBibliographyEntry};
use crate::inline::Inline;
use crate::parser::AstDocument;

use super::body::{self, BlockWriter, classes, passive};
use super::{bibliography_reference_id, render_input_diagnostic, safe};

#[derive(Clone, Debug)]
pub(super) struct PreparedGeneratedBibliography<'input> {
    title: &'input str,
    entries: Vec<PreparedGeneratedBibliographyEntry<'input>>,
    entry_by_key: BTreeMap<&'input str, usize>,
    numbered: bool,
}

#[derive(Clone, Debug)]
pub(super) struct PreparedGeneratedBibliographyEntry<'input> {
    pub(super) input: &'input GeneratedBibliographyEntry,
    references: Vec<crate::source::TextRange>,
}

impl PreparedGeneratedBibliography<'_> {
    pub(super) fn entry(&self, key: &str) -> Option<&PreparedGeneratedBibliographyEntry<'_>> {
        self.entry_by_key
            .get(key)
            .and_then(|index| self.entries.get(*index))
    }

    pub(super) fn defines(&self, key: &str) -> bool {
        self.entry_by_key.contains_key(key)
    }
}

pub(super) fn prepare<'input>(
    bibliography: Option<&'input GeneratedBibliography>,
    document: &AstDocument,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<PreparedGeneratedBibliography<'input>> {
    let bibliography = bibliography?;
    let diagnostic_range =
        crate::source::TextRange::new(crate::source::TextSize::ZERO, crate::source::TextSize::ZERO)
            .expect("the zero range is ordered");
    if bibliography.title().trim().is_empty() {
        diagnostics.push(render_input_diagnostic(
            "invalid-generated-bibliography",
            "generated bibliography",
            "generated bibliography title must not be empty",
            diagnostic_range,
        ));
        return None;
    }

    let mut entries = Vec::new();
    let mut entry_by_key = BTreeMap::new();
    for (entry_index, entry) in bibliography.entries().iter().enumerate() {
        let key = entry.citation_key();
        let diagnostic_domain = format!("generated bibliography entry {entry_index}");
        if !crate::block_grammar::valid_anchor_id(key) {
            diagnostics.push(render_input_diagnostic(
                "invalid-generated-bibliography-entry",
                &diagnostic_domain,
                "generated bibliography citation key is not a valid anchor identifier",
                diagnostic_range,
            ));
            continue;
        }
        if entry_by_key.contains_key(key) {
            diagnostics.push(render_input_diagnostic(
                "duplicate-generated-bibliography-entry",
                &diagnostic_domain,
                &format!("generated bibliography contains the citation key `{key}` more than once"),
                diagnostic_range,
            ));
            continue;
        }
        if document.identifiers().target_by_id(key).is_some() {
            diagnostics.push(render_input_diagnostic(
                "shadowed-generated-bibliography-entry",
                &diagnostic_domain,
                &format!(
                    "the document definition of `{key}` takes precedence over the generated entry"
                ),
                diagnostic_range,
            ));
            continue;
        }
        entry_by_key.insert(key, entries.len());
        entries.push(PreparedGeneratedBibliographyEntry {
            input: entry,
            references: Vec::new(),
        });
    }

    crate::walker::walk_ast(document, |node| {
        let crate::walker::SemanticNode::Inline(Inline::Macro(node)) = node else {
            return;
        };
        if node.kind != crate::inline::StandardMacroKind::Citation {
            return;
        }
        for key in node.attributes.iter().filter(|key| key.name.is_none()) {
            if let Some(index) = entry_by_key.get(key.value.as_str()).copied() {
                entries[index].references.push(key.value_range);
            }
        }
    });

    for (entry_index, entry) in entries.iter().enumerate() {
        if entry.references.is_empty() {
            diagnostics.push(render_input_diagnostic(
                "unused-generated-bibliography-entry",
                &format!("generated bibliography entry {entry_index}"),
                &format!(
                    "generated bibliography entry `{}` is not cited by the document",
                    entry.input.citation_key()
                ),
                diagnostic_range,
            ));
        }
    }

    if entries.is_empty() {
        return None;
    }
    let numbered = match numbering(&entries) {
        Ok(numbered) => numbered,
        Err(reason) => {
            let mut diagnostic = render_input_diagnostic(
                "invalid-generated-bibliography-numbering",
                "generated bibliography",
                &format!(
                    "generated bibliography is not rendered because {reason}; the numbers left \
                     after invalid, duplicate and shadowed entries are dropped must read 1, 2, …, \
                     n in order, or no entry may carry a number"
                ),
                diagnostic_range,
            );
            diagnostic.severity = crate::diagnostic::Severity::Error;
            diagnostics.push(diagnostic);
            return None;
        }
    };

    Some(PreparedGeneratedBibliography {
        title: bibliography.title(),
        entries,
        entry_by_key,
        numbered,
    })
}

/// Decides whether the surviving entries form a numbered bibliography.
///
/// Every entry carries a number or none does, and the numbers that survive must
/// count up from one without a gap. Anything else means the list would disagree
/// with the numbers the document already shows for its citations, so it is
/// reported instead of rendered.
fn numbering(entries: &[PreparedGeneratedBibliographyEntry<'_>]) -> Result<bool, String> {
    let numbered = entries
        .iter()
        .filter(|entry| entry.input.number().is_some());
    let count = numbered.count();
    if count == 0 {
        return Ok(false);
    }
    if count < entries.len() {
        return Err(format!(
            "only {count} of its {} entries carry a number",
            entries.len()
        ));
    }
    for (index, entry) in entries.iter().enumerate() {
        let expected = u32::try_from(index + 1)
            .map_err(|_| "it has more entries than a bibliography number can count".to_owned())?;
        let actual = entry.input.number().expect("every entry carries a number");
        if actual != expected {
            return Err(format!(
                "entry `{}` is numbered {actual} where {expected} was expected",
                entry.input.citation_key()
            ));
        }
    }
    Ok(true)
}

pub(super) fn render(output: &mut String, bibliography: &PreparedGeneratedBibliography<'_>) {
    BlockWriter::start(output, "div", &[]);
    BlockWriter::line_break(output);
    BlockWriter::start(output, "h2", &[]);
    BlockWriter::inline_text(output, bibliography.title);
    BlockWriter::end(output, "h2");
    BlockWriter::line_break(output);
    let list = if bibliography.numbered { "ol" } else { "ul" };
    BlockWriter::start(output, list, &[]);
    BlockWriter::line_break(output);
    for entry in &bibliography.entries {
        BlockWriter::start(output, "li", &[]);
        BlockWriter::start(
            output,
            "span",
            &[
                passive("id", entry.input.citation_key()),
                classes(&["bibliography-anchor"]),
            ],
        );
        BlockWriter::end(output, "span");
        BlockWriter::inline_text(output, entry.input.text());
        for (index, reference) in entry.references.iter().enumerate() {
            BlockWriter::text(output, " ");
            let target = bibliography_reference_id(*reference);
            let href = safe::SafeFragmentUrl::new(&target)
                .expect("generated bibliography reference IDs are control-free")
                .into_owned();
            BlockWriter::start(
                output,
                "a",
                &[
                    classes(&["bibliography-backref"]),
                    body::fragment_url("href", href),
                ],
            );
            BlockWriter::text(output, &format!("↩{}", index + 1));
            BlockWriter::end(output, "a");
        }
        BlockWriter::end(output, "li");
        BlockWriter::line_break(output);
    }
    BlockWriter::end(output, list);
    BlockWriter::line_break(output);
    BlockWriter::end(output, "div");
    BlockWriter::line_break(output);
}
