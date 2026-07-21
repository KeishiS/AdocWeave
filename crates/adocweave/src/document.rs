//! Output-independent document indexes and editor-facing symbols.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::parser::{
    AstBlock, AstDocument, ElementAttribute, Heading, HeadingKind, MetadataValue, SourceBlock,
};
use crate::source::TextRange;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeadingId {
    pub range: TextRange,
    pub base: String,
    pub id: String,
}

pub fn generate_heading_ids(document: &AstDocument) -> Vec<HeadingId> {
    let mut occurrences = BTreeMap::<String, usize>::new();
    let mut used = document
        .anchors()
        .iter()
        .filter(|anchor| anchor.valid)
        .map(|anchor| anchor.id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    document
        .blocks()
        .iter()
        .filter_map(|block| match block {
            AstBlock::Heading(heading) => {
                let base = heading_id_base(&heading.text);
                let explicit = document
                    .anchors()
                    .iter()
                    .find(|anchor| anchor.valid && anchor.target_range == Some(heading.range));
                let id = explicit.map_or_else(
                    || {
                        let occurrence = occurrences.entry(base.clone()).or_default();
                        loop {
                            *occurrence += 1;
                            let candidate = if *occurrence == 1 {
                                base.clone()
                            } else {
                                format!("{base}_{}", *occurrence)
                            };
                            if used.insert(candidate.clone()) {
                                break candidate;
                            }
                        }
                    },
                    |anchor| anchor.id.clone(),
                );
                Some(HeadingId {
                    range: explicit.map_or(heading.text_range, |anchor| anchor.id_range),
                    base,
                    id,
                })
            }
            _ => None,
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferenceTargetKind {
    DocumentTitle,
    Section,
    ExplicitAnchor,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferenceTarget {
    pub kind: ReferenceTargetKind,
    pub id: String,
    pub label: String,
    pub id_range: TextRange,
    pub target_range: TextRange,
}

pub fn reference_targets(document: &AstDocument) -> Vec<ReferenceTarget> {
    let heading_ids = generate_heading_ids(document);
    let mut heading_index = 0;
    let mut targets = Vec::new();
    for block in document.blocks() {
        let range = block.range();
        let attached = document
            .anchors()
            .iter()
            .filter(|anchor| anchor.valid && anchor.target_range == Some(range))
            .collect::<Vec<_>>();
        for anchor in &attached {
            targets.push(ReferenceTarget {
                kind: match block {
                    AstBlock::Heading(heading) => match heading.kind {
                        HeadingKind::DocumentTitle => ReferenceTargetKind::DocumentTitle,
                        HeadingKind::Section { .. } => ReferenceTargetKind::Section,
                    },
                    _ => ReferenceTargetKind::ExplicitAnchor,
                },
                id: anchor.id.clone(),
                label: anchor.label.clone().unwrap_or_else(|| block_label(block)),
                id_range: anchor.id_range,
                target_range: range,
            });
        }
        if let AstBlock::Heading(heading) = block {
            let generated = &heading_ids[heading_index];
            heading_index += 1;
            if attached.is_empty() {
                targets.push(ReferenceTarget {
                    kind: match heading.kind {
                        HeadingKind::DocumentTitle => ReferenceTargetKind::DocumentTitle,
                        HeadingKind::Section { .. } => ReferenceTargetKind::Section,
                    },
                    id: generated.id.clone(),
                    label: heading.text.clone(),
                    id_range: generated.range,
                    target_range: heading.range,
                });
            }
        }
    }
    targets
}

fn block_label(block: &AstBlock) -> String {
    match block {
        AstBlock::Heading(value) => value.text.clone(),
        AstBlock::Paragraph(value) => value.value.lines().next().unwrap_or_default().to_owned(),
        AstBlock::Literal(_) => "literal block".to_owned(),
        AstBlock::Source(value) => value.language.as_ref().map_or_else(
            || "source block".to_owned(),
            |name| format!("{name} source block"),
        ),
        AstBlock::List(value) => value
            .items
            .first()
            .map_or_else(|| "list".to_owned(), |item| item.text.clone()),
        AstBlock::Math(_) => "math block".to_owned(),
        AstBlock::Delimited(value) => format!("{:?} block", value.kind).to_ascii_lowercase(),
        AstBlock::Unsupported(_) => "unsupported block".to_owned(),
    }
}

pub fn heading_id_base(text: &str) -> String {
    let mut id = String::from("_");
    let mut pending_separator = false;
    for character in text.chars() {
        if character.is_alphanumeric() {
            if pending_separator && id.len() > 1 {
                id.push('_');
            }
            for lower in character.to_lowercase() {
                id.push(lower);
            }
            pending_separator = false;
        } else {
            pending_separator = true;
        }
    }
    if id.len() == 1 {
        id.push_str("section");
    }
    id
}

pub fn source_language_candidates(prefix: &str) -> Vec<&'static str> {
    const LANGUAGES: [&str; 12] = [
        "bash",
        "c",
        "cpp",
        "css",
        "html",
        "javascript",
        "json",
        "python",
        "rust",
        "sql",
        "typescript",
        "yaml",
    ];
    let prefix = prefix.to_ascii_lowercase();
    LANGUAGES
        .into_iter()
        .filter(|language| language.starts_with(&prefix))
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentElement<'document> {
    HeadingMarker(&'document Heading),
    HeadingText(&'document Heading),
    SourceLanguage(&'document SourceBlock),
    SourceAttribute(&'document SourceBlock),
    MetadataTitle(&'document MetadataValue),
    MetadataId(&'document MetadataValue),
    MetadataRole(&'document MetadataValue),
    MetadataOption(&'document MetadataValue),
    ElementAttribute(&'document ElementAttribute),
}

pub fn document_element_at(document: &AstDocument, offset: u32) -> Option<DocumentElement<'_>> {
    document.blocks().iter().find_map(|block| {
        let structural = match block {
            AstBlock::Heading(heading) if contains(heading.marker_range, offset, false) => {
                Some(DocumentElement::HeadingMarker(heading))
            }
            AstBlock::Heading(heading) if contains(heading.text_range, offset, true) => {
                Some(DocumentElement::HeadingText(heading))
            }
            AstBlock::Source(source)
                if source
                    .language_range
                    .is_some_and(|range| contains(range, offset, true)) =>
            {
                Some(DocumentElement::SourceLanguage(source))
            }
            AstBlock::Source(source) if contains(source.attribute_range, offset, false) => {
                Some(DocumentElement::SourceAttribute(source))
            }
            _ => None,
        };
        structural.or_else(|| {
            let metadata = block.metadata();
            metadata
                .title
                .as_ref()
                .filter(|value| contains(value.range, offset, true))
                .map(DocumentElement::MetadataTitle)
                .or_else(|| {
                    metadata
                        .id
                        .as_ref()
                        .filter(|value| contains(value.range, offset, true))
                        .map(DocumentElement::MetadataId)
                })
                .or_else(|| {
                    metadata
                        .roles
                        .iter()
                        .find(|value| contains(value.range, offset, true))
                        .map(DocumentElement::MetadataRole)
                })
                .or_else(|| {
                    metadata
                        .options
                        .iter()
                        .find(|value| contains(value.range, offset, true))
                        .map(DocumentElement::MetadataOption)
                })
                .or_else(|| {
                    metadata
                        .attributes
                        .iter()
                        .find(|value| contains(value.range, offset, true))
                        .map(DocumentElement::ElementAttribute)
                })
        })
    })
}

fn contains(range: TextRange, offset: u32, include_end: bool) -> bool {
    range.start().to_u32() <= offset
        && if include_end {
            offset <= range.end().to_u32()
        } else {
            offset < range.end().to_u32()
        }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SymbolKind {
    DocumentTitle,
    Section,
    ListItem,
}

impl SymbolKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::DocumentTitle => "document-title",
            Self::Section => "section",
            Self::ListItem => "list-item",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentSymbol {
    pub name: String,
    pub kind: SymbolKind,
    pub range: TextRange,
    pub selection_range: TextRange,
    pub children: Vec<DocumentSymbol>,
}

#[derive(Debug)]
struct ArenaSymbol {
    symbol: DocumentSymbol,
    parent: Option<usize>,
}

pub fn document_symbols(document: &AstDocument) -> Vec<DocumentSymbol> {
    let mut arena = Vec::<ArenaSymbol>::new();
    let mut section_stack = Vec::<(u8, usize)>::new();
    let mut title_index = None;

    for block in document.blocks() {
        let AstBlock::Heading(heading) = block else {
            if let AstBlock::List(list) = block {
                let parent = section_stack
                    .last()
                    .map(|(_, index)| *index)
                    .or(title_index);
                append_list_symbols(&mut arena, list, parent);
            }
            continue;
        };
        match heading.kind {
            HeadingKind::DocumentTitle => {
                let index = arena.len();
                arena.push(ArenaSymbol {
                    symbol: DocumentSymbol {
                        name: heading.text.clone(),
                        kind: SymbolKind::DocumentTitle,
                        range: heading.range,
                        selection_range: heading.text_range,
                        children: Vec::new(),
                    },
                    parent: None,
                });
                title_index = Some(index);
                section_stack.clear();
            }
            HeadingKind::Section { level } => {
                while section_stack
                    .last()
                    .is_some_and(|(ancestor_level, _)| *ancestor_level >= level)
                {
                    section_stack.pop();
                }
                let parent = section_stack
                    .last()
                    .map(|(_, index)| *index)
                    .or(title_index);
                let index = arena.len();
                arena.push(ArenaSymbol {
                    symbol: DocumentSymbol {
                        name: heading.text.clone(),
                        kind: SymbolKind::Section,
                        range: heading.range,
                        selection_range: heading.text_range,
                        children: Vec::new(),
                    },
                    parent,
                });
                section_stack.push((level, index));
            }
        }
    }

    let mut roots = Vec::new();
    for index in (0..arena.len()).rev() {
        let parent = arena[index].parent;
        let range = arena[index].symbol.range;
        let selection_range = arena[index].symbol.selection_range;
        let symbol = std::mem::replace(
            &mut arena[index].symbol,
            DocumentSymbol {
                name: String::new(),
                kind: SymbolKind::Section,
                range,
                selection_range,
                children: Vec::new(),
            },
        );
        if let Some(parent) = parent {
            arena[parent].symbol.children.insert(0, symbol);
        } else {
            roots.insert(0, symbol);
        }
    }
    roots
}

fn append_list_symbols(
    arena: &mut Vec<ArenaSymbol>,
    list: &crate::parser::ListBlock,
    parent: Option<usize>,
) {
    for item in &list.items {
        let index = arena.len();
        arena.push(ArenaSymbol {
            symbol: DocumentSymbol {
                name: item.text.clone(),
                kind: SymbolKind::ListItem,
                range: item.range,
                selection_range: item.text_range,
                children: Vec::new(),
            },
            parent,
        });
        for child in &item.children {
            append_list_symbols(arena, child, Some(index));
        }
    }
}

pub fn render_symbols_json(symbols: &[DocumentSymbol]) -> String {
    fn render(output: &mut String, symbols: &[DocumentSymbol]) {
        output.push('[');
        for (index, symbol) in symbols.iter().enumerate() {
            if index != 0 {
                output.push(',');
            }
            write!(output, "{{\"name\":",).expect("writing to a String cannot fail");
            write_json_string(output, &symbol.name);
            write!(
                output,
                ",\"kind\":\"{}\",\"range\":{{\"start\":{},\"end\":{}}},\
                 \"selectionRange\":{{\"start\":{},\"end\":{}}},\"children\":",
                symbol.kind.as_str(),
                symbol.range.start().to_u32(),
                symbol.range.end().to_u32(),
                symbol.selection_range.start().to_u32(),
                symbol.selection_range.end().to_u32()
            )
            .expect("writing to a String cannot fail");
            render(output, &symbol.children);
            output.push('}');
        }
        output.push(']');
    }

    let mut output = String::new();
    render(&mut output, symbols);
    output
}

fn write_json_string(output: &mut String, value: &str) {
    crate::json::write_string(output, value);
}

#[cfg(test)]
mod tests {
    use super::{
        DocumentElement, ReferenceTargetKind, document_element_at, document_symbols,
        generate_heading_ids, reference_targets, render_symbols_json, source_language_candidates,
    };
    use crate::parser::parse;

    #[test]
    fn source_block_language_candidates_are_deterministic_and_filtered() {
        assert_eq!(source_language_candidates("ru"), ["rust"]);
        assert_eq!(
            source_language_candidates(""),
            source_language_candidates("")
        );
    }

    #[test]
    fn document_element_at_distinguishes_heading_and_source_parts() {
        let source = "= 題名😀\n\n[source, ru]\n----\ncode\n----\n";
        let parsed = parse(source).expect("valid source");

        assert!(matches!(
            super::document_element_at(&parsed.ast, 0),
            Some(super::DocumentElement::HeadingMarker(_))
        ));
        assert!(matches!(
            super::document_element_at(&parsed.ast, 2),
            Some(super::DocumentElement::HeadingText(_))
        ));
        let language_end = source.find("ru]").expect("language") as u32 + 2;
        assert!(matches!(
            super::document_element_at(&parsed.ast, language_end),
            Some(super::DocumentElement::SourceLanguage(_))
        ));
        assert!(super::document_element_at(&parsed.ast, 13).is_none());
    }

    #[test]
    fn document_element_at_queries_common_block_metadata() {
        let source = ".Visible\n[#item.lead%collapsible,cols=2]\nParagraph\n";
        let parsed = parse(source).expect("valid source");

        for (needle, expected) in [
            ("Visible", "title"),
            ("item", "id"),
            ("lead", "role"),
            ("collapsible", "option"),
            ("cols=2", "attribute"),
        ] {
            let offset =
                u32::try_from(source.find(needle).expect("fixture value")).expect("offset");
            let actual = match document_element_at(&parsed.ast, offset) {
                Some(DocumentElement::MetadataTitle(_)) => "title",
                Some(DocumentElement::MetadataId(_)) => "id",
                Some(DocumentElement::MetadataRole(_)) => "role",
                Some(DocumentElement::MetadataOption(_)) => "option",
                Some(DocumentElement::ElementAttribute(_)) => "attribute",
                other => panic!("unexpected element at {needle}: {other:?}"),
            };
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn document_symbols_follow_heading_hierarchy() {
        let parsed = parse("= Title\n\n== One\n=== Child\n== Two").expect("valid source");
        let symbols = document_symbols(&parsed.ast);

        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "Title");
        assert_eq!(symbols[0].children.len(), 2);
        assert_eq!(symbols[0].children[0].name, "One");
        assert_eq!(symbols[0].children[0].children[0].name, "Child");
        assert_eq!(symbols[0].children[1].name, "Two");
    }

    #[test]
    fn document_symbols_and_ids_are_deterministic() {
        let parsed = parse("== Same\n== Same").expect("valid source");

        assert_eq!(
            generate_heading_ids(&parsed.ast)
                .iter()
                .map(|heading| heading.id.as_str())
                .collect::<Vec<_>>(),
            ["_same", "_same_2"]
        );
        assert_eq!(
            render_symbols_json(&document_symbols(&parsed.ast)),
            render_symbols_json(&document_symbols(&parsed.ast))
        );
    }

    #[test]
    fn anchors_create_stable_reference_targets_and_override_heading_ids() {
        let parsed =
            parse("= Title\n\n[[stable,表示名]]\n== Generated title\n\n[#paragraph]\nParagraph\n")
                .expect("parse");
        let targets = reference_targets(&parsed.ast);

        assert_eq!(
            targets
                .iter()
                .map(|target| (target.kind, target.id.as_str(), target.label.as_str()))
                .collect::<Vec<_>>(),
            [
                (ReferenceTargetKind::DocumentTitle, "_title", "Title"),
                (ReferenceTargetKind::Section, "stable", "表示名"),
                (
                    ReferenceTargetKind::ExplicitAnchor,
                    "paragraph",
                    "Paragraph"
                ),
            ]
        );
        assert_eq!(
            generate_heading_ids(&parsed.ast)
                .iter()
                .map(|heading| heading.id.as_str())
                .collect::<Vec<_>>(),
            ["_title", "stable"]
        );
    }

    #[test]
    fn anchors_keep_unicode_combining_emoji_and_case_distinct() {
        let parsed =
            parse(include_str!("../../../fixtures/anchors/boundaries.adoc")).expect("parse");
        let ids = reference_targets(&parsed.ast)
            .into_iter()
            .map(|target| target.id)
            .collect::<Vec<_>>();

        assert_eq!(ids, ["_文書", "日本語", "Café", "😀", "Case", "case"]);
        assert_eq!(
            reference_targets(&parsed.ast),
            reference_targets(&parsed.ast)
        );
    }
}
