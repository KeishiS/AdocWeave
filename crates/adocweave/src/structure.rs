//! Shared doctype-aware document structure projection.

use crate::parser::{AstBlock, AstDocument, DocumentType, Heading, HeadingKind};
use crate::source::TextRange;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DocumentStructure {
    roots: Vec<Section>,
    headings: Vec<StructuredHeading>,
    manpage: Option<Manpage>,
    problems: Vec<StructureProblem>,
}

impl DocumentStructure {
    pub fn roots(&self) -> &[Section] {
        &self.roots
    }

    pub fn headings(&self) -> &[StructuredHeading] {
        &self.headings
    }

    pub const fn manpage(&self) -> Option<&Manpage> {
        self.manpage.as_ref()
    }

    pub fn problems(&self) -> &[StructureProblem] {
        &self.problems
    }

    pub fn heading_at(&self, range: TextRange) -> Option<&StructuredHeading> {
        self.headings.iter().find(|heading| heading.range == range)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SectionKind {
    DocumentTitle,
    Part,
    Section,
    Appendix,
    Discrete,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StructuredHeading {
    pub kind: SectionKind,
    pub level: u8,
    pub id: String,
    pub id_range: TextRange,
    pub title: String,
    pub range: TextRange,
    pub title_range: TextRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Section {
    pub heading: StructuredHeading,
    pub children: Vec<Section>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TocEntry {
    pub id: String,
    pub title: String,
    pub level: u8,
    pub number: Vec<u32>,
    pub range: TextRange,
    pub children: Vec<TocEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Manpage {
    pub name: String,
    pub section: String,
    pub purpose: String,
    pub title_range: TextRange,
    pub name_range: TextRange,
    pub purpose_range: TextRange,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StructureProblemKind {
    AppendixLevel,
    AppendixDoctype,
    BibliographyNotSection,
    BibliographyScope,
    BibliographyDoctype,
    MissingManpageTitle,
    InvalidManpageTitle,
    MissingManpageNameSection,
    InvalidManpagePurpose,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StructureProblem {
    pub kind: StructureProblemKind,
    pub range: TextRange,
}

#[derive(Debug)]
struct ArenaSection {
    heading: StructuredHeading,
    parent: Option<usize>,
    children: Vec<usize>,
}

pub(crate) fn build(
    document: &AstDocument,
    identifiers: &crate::document::DocumentIdentifiers,
) -> DocumentStructure {
    let mut structure = DocumentStructure::default();
    let mut arena = Vec::<ArenaSection>::new();
    let mut stack = Vec::<(u8, usize)>::new();
    let mut title = None;
    let multipart_book = document.header().doctype == DocumentType::Book
        && document.blocks().iter().any(|block| {
            matches!(block, AstBlock::Heading(heading) if heading.kind == HeadingKind::Part && !is_bibliography(heading))
        });

    for block in document.blocks() {
        let AstBlock::Heading(heading) = block else {
            continue;
        };
        let identifier = identifiers
            .heading_at(heading.text_range)
            .expect("lowering assigns every heading an identifier");
        let id = identifier.id.clone();
        let id_range = identifiers
            .targets()
            .iter()
            .find(|target| target.target_range == heading.range && target.id == id)
            .map_or(heading.text_range, |target| target.id_range);
        let appendix = is_appendix(heading);
        let bibliography = is_bibliography(heading);
        let (kind, level) = match heading.kind {
            HeadingKind::DocumentTitle => (SectionKind::DocumentTitle, 0),
            HeadingKind::Part => (SectionKind::Part, 0),
            HeadingKind::Section { level } if appendix => (SectionKind::Appendix, level),
            HeadingKind::Section { level } => (SectionKind::Section, level),
            HeadingKind::Discrete { level } => (SectionKind::Discrete, level),
        };
        if kind == SectionKind::Appendix {
            if level != 1 {
                structure.problems.push(StructureProblem {
                    kind: StructureProblemKind::AppendixLevel,
                    range: heading.range,
                });
            }
            if !matches!(
                document.header().doctype,
                DocumentType::Article | DocumentType::Book
            ) {
                structure.problems.push(StructureProblem {
                    kind: StructureProblemKind::AppendixDoctype,
                    range: heading.range,
                });
            }
        }
        if bibliography {
            if matches!(
                heading.kind,
                HeadingKind::DocumentTitle | HeadingKind::Discrete { .. }
            ) {
                structure.problems.push(StructureProblem {
                    kind: StructureProblemKind::BibliographyNotSection,
                    range: heading.range,
                });
            }
            if heading.kind == HeadingKind::Part && !multipart_book {
                structure.problems.push(StructureProblem {
                    kind: StructureProblemKind::BibliographyScope,
                    range: heading.range,
                });
            }
            if !matches!(heading.kind, HeadingKind::Section { level: 2.. })
                && !matches!(
                    document.header().doctype,
                    DocumentType::Article | DocumentType::Book
                )
            {
                structure.problems.push(StructureProblem {
                    kind: StructureProblemKind::BibliographyDoctype,
                    range: heading.range,
                });
            }
        }
        let structured = StructuredHeading {
            kind,
            level,
            id,
            id_range,
            title: heading.text.clone(),
            range: heading.range,
            title_range: heading.text_range,
        };
        structure.headings.push(structured.clone());
        if kind == SectionKind::Discrete {
            continue;
        }
        let hierarchy_level = if kind == SectionKind::DocumentTitle {
            0
        } else {
            level
        };
        while stack
            .last()
            .is_some_and(|(ancestor_level, _)| *ancestor_level >= hierarchy_level)
        {
            stack.pop();
        }
        let parent = if kind == SectionKind::DocumentTitle {
            None
        } else if kind == SectionKind::Part {
            title
        } else {
            stack.last().map(|(_, index)| *index).or(title)
        };
        let index = arena.len();
        arena.push(ArenaSection {
            heading: structured,
            parent,
            children: Vec::new(),
        });
        if let Some(parent) = parent {
            arena[parent].children.push(index);
        }
        if kind == SectionKind::DocumentTitle {
            title = Some(index);
            stack.clear();
        } else if kind == SectionKind::Part {
            stack.clear();
            stack.push((0, index));
        } else {
            stack.push((hierarchy_level, index));
        }
    }
    structure.roots = arena
        .iter()
        .enumerate()
        .filter(|(_, node)| node.parent.is_none())
        .map(|(index, _)| materialize_section(index, &arena))
        .collect();
    if document.header().doctype == DocumentType::Manpage {
        structure.manpage = build_manpage(document, &mut structure.problems);
    }
    structure
}

fn is_appendix(heading: &Heading) -> bool {
    heading
        .metadata
        .attributes
        .iter()
        .any(|attribute| attribute.name.is_none() && attribute.value == "appendix")
        || heading
            .metadata
            .roles
            .iter()
            .any(|role| role.value == "appendix")
}

fn is_bibliography(heading: &Heading) -> bool {
    heading
        .metadata
        .attributes
        .iter()
        .any(|attribute| attribute.name.is_none() && attribute.value == "bibliography")
}

fn materialize_section(index: usize, arena: &[ArenaSection]) -> Section {
    Section {
        heading: arena[index].heading.clone(),
        children: arena[index]
            .children
            .iter()
            .map(|child| materialize_section(*child, arena))
            .collect(),
    }
}

fn build_manpage(document: &AstDocument, problems: &mut Vec<StructureProblem>) -> Option<Manpage> {
    let title = document.blocks().iter().find_map(|block| match block {
        AstBlock::Heading(heading) if heading.kind == HeadingKind::DocumentTitle => Some(heading),
        _ => None,
    });
    let Some(title) = title else {
        problems.push(StructureProblem {
            kind: StructureProblemKind::MissingManpageTitle,
            range: TextRange::new(document.header().end, document.header().end)
                .expect("empty header range"),
        });
        return None;
    };
    let Some((name, section)) = title
        .text
        .strip_suffix(')')
        .and_then(|value| value.rsplit_once('('))
        .filter(|(name, section)| !name.is_empty() && !section.is_empty())
    else {
        problems.push(StructureProblem {
            kind: StructureProblemKind::InvalidManpageTitle,
            range: title.text_range,
        });
        return None;
    };
    let name_heading = document.blocks().iter().position(|block| {
        matches!(block, AstBlock::Heading(heading) if heading.text.eq_ignore_ascii_case("NAME"))
    });
    let Some(index) = name_heading else {
        problems.push(StructureProblem {
            kind: StructureProblemKind::MissingManpageNameSection,
            range: title.range,
        });
        return None;
    };
    let Some(AstBlock::Paragraph(paragraph)) = document.blocks().get(index + 1) else {
        problems.push(StructureProblem {
            kind: StructureProblemKind::InvalidManpagePurpose,
            range: document.blocks()[index].range(),
        });
        return None;
    };
    let Some((declared_name, purpose)) = paragraph
        .value
        .split_once(" - ")
        .or_else(|| paragraph.value.split_once(" — "))
        .filter(|(declared_name, purpose)| *declared_name == name && !purpose.is_empty())
    else {
        problems.push(StructureProblem {
            kind: StructureProblemKind::InvalidManpagePurpose,
            range: paragraph.content_range,
        });
        return None;
    };
    let name_offset = paragraph.value.find(declared_name).unwrap_or(0);
    Some(Manpage {
        name: name.to_owned(),
        section: section.to_owned(),
        purpose: purpose.to_owned(),
        title_range: title.text_range,
        name_range: TextRange::new(
            crate::source::TextSize::new(paragraph.content_range.start().to_usize() + name_offset)
                .expect("manpage name start"),
            crate::source::TextSize::new(
                paragraph.content_range.start().to_usize() + name_offset + declared_name.len(),
            )
            .expect("manpage name end"),
        )
        .expect("manpage name range"),
        purpose_range: TextRange::new(
            crate::source::TextSize::new(paragraph.content_range.end().to_usize() - purpose.len())
                .expect("manpage purpose start"),
            paragraph.content_range.end(),
        )
        .expect("manpage purpose range"),
    })
}

#[cfg(test)]
mod tests {
    use crate::parser::parse;

    #[test]
    fn section_tree_ids_toc_numbers_and_exclusions_share_one_projection() {
        let parsed = parse(
            "\
= Title

== One
=== Child

[.notoc]
== Hidden

== Two
",
        )
        .expect("parse");
        let structure = parsed.ast.structure();
        assert_eq!(structure.roots().len(), 1);
        assert_eq!(structure.roots()[0].children.len(), 3);
        assert_eq!(structure.headings()[1].id, "_one");
        let presentation = parsed.ast.presentation();
        assert_eq!(
            presentation
                .heading_at(structure.headings()[2].range)
                .unwrap()
                .number,
            [1, 1]
        );
        assert_eq!(presentation.toc().len(), 2);
        assert_eq!(presentation.toc()[0].title, "One");
        assert_eq!(presentation.toc()[0].children[0].title, "Child");
        assert_eq!(presentation.toc()[1].title, "Two");
    }

    #[test]
    fn book_parts_and_appendices_are_typed_and_validated() {
        let parsed = parse(
            "\
= Book
:doctype: book

= Part

== Chapter

[appendix]
== Reference
",
        )
        .expect("parse");
        let headings = parsed.ast.structure().headings();
        assert_eq!(headings[1].kind, super::SectionKind::Part);
        assert_eq!(headings[3].kind, super::SectionKind::Appendix);
        assert!(parsed.ast.structure().problems().is_empty());

        let invalid = parse(
            "\
= Tool(1)
:doctype: manpage

[appendix]
=== Bad
",
        )
        .expect("parse");
        assert!(
            invalid
                .ast
                .structure()
                .problems()
                .iter()
                .any(|problem| { problem.kind == super::StructureProblemKind::AppendixLevel })
        );
    }

    #[test]
    fn manpage_name_section_and_purpose_are_structured() {
        let parsed = parse(
            "\
= adocweave(1)
:doctype: manpage

== NAME

adocweave - convert AsciiDoc safely
",
        )
        .expect("parse");
        let manpage = parsed.ast.structure().manpage().expect("manpage");
        assert_eq!(manpage.name, "adocweave");
        assert_eq!(manpage.section, "1");
        assert_eq!(manpage.purpose, "convert AsciiDoc safely");
        assert!(parsed.ast.structure().problems().is_empty());
    }
}
