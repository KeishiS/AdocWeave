//! HTML output backend.
//!
//! This module depends on the output-neutral semantic AST. The parser and AST
//! do not depend on this module, so additional output backends can consume the
//! same document without changing parsing behavior.

use std::collections::BTreeMap;

use crate::diagnostic::Diagnostic;
use crate::document::{HeadingId, generate_heading_ids};
use crate::parser::{AstBlock, AstDocument, Heading, HeadingKind, Paragraph, Unsupported};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HtmlOptions {
    pub complete_document: bool,
    pub render_document_title: bool,
}

impl Default for HtmlOptions {
    fn default() -> Self {
        Self {
            complete_document: false,
            render_document_title: true,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HtmlOutput {
    pub html: String,
    pub diagnostics: Vec<Diagnostic>,
    pub document_attributes: BTreeMap<String, String>,
    pub heading_ids: Vec<HeadingId>,
}

pub fn render(document: &AstDocument, options: &HtmlOptions) -> HtmlOutput {
    let mut fragment = String::new();
    let heading_ids = generate_heading_ids(document);
    let mut heading_index = 0;
    for block in &document.blocks {
        match block {
            AstBlock::Heading(heading) => {
                let id = &heading_ids[heading_index].id;
                heading_index += 1;
                render_heading(&mut fragment, heading, id, options);
            }
            AstBlock::Paragraph(paragraph) => render_paragraph(&mut fragment, paragraph),
            AstBlock::Unsupported(unsupported) => render_unsupported(&mut fragment, unsupported),
        }
    }

    let html = if options.complete_document {
        format!("<!doctype html>\n<html>\n<body>\n{fragment}</body>\n</html>\n")
    } else {
        fragment
    };

    HtmlOutput {
        html,
        diagnostics: Vec::new(),
        document_attributes: BTreeMap::new(),
        heading_ids,
    }
}

fn render_heading(output: &mut String, heading: &Heading, id: &str, options: &HtmlOptions) {
    if !heading.problems.is_empty() {
        output.push_str("<p>");
        escape_html_into(output, &heading.text);
        output.push_str("</p>\n");
        return;
    }

    match heading.kind {
        HeadingKind::DocumentTitle if options.render_document_title => {
            output.push_str("<h1 class=\"document-title\" id=\"");
            output.push_str(id);
            output.push_str("\">");
            escape_html_into(output, &heading.text);
            output.push_str("</h1>\n");
        }
        HeadingKind::DocumentTitle => {}
        HeadingKind::Section { level } => {
            let level = char::from(b'0' + level);
            output.push_str("<h");
            output.push(level);
            output.push_str(" id=\"");
            output.push_str(id);
            output.push_str("\">");
            escape_html_into(output, &heading.text);
            output.push_str("</h");
            output.push(level);
            output.push_str(">\n");
        }
    }
}

fn render_paragraph(output: &mut String, paragraph: &Paragraph) {
    output.push_str("<p>");
    for (index, line) in paragraph.lines.iter().enumerate() {
        if index != 0 {
            output.push(' ');
        }
        escape_html_into(output, &line.value);
    }
    output.push_str("</p>\n");
}

fn render_unsupported(output: &mut String, unsupported: &Unsupported) {
    output.push_str("<p>");
    escape_html_into(output, &unsupported.raw);
    output.push_str("</p>\n");
}

fn escape_html_into(output: &mut String, text: &str) {
    for character in text.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&#34;"),
            '\'' => output.push_str("&#39;"),
            _ => output.push(character),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{HtmlOptions, render};
    use crate::parser::parse;

    #[test]
    fn html_renderer_renders_paragraphs_and_folds_source_lines() {
        let parsed = parse("first line\nsecond line\n\nlast").expect("valid source");

        assert_eq!(
            render(&parsed.ast, &HtmlOptions::default()).html,
            "<p>first line second line</p>\n<p>last</p>\n"
        );
    }

    #[test]
    fn html_renderer_escapes_all_special_characters_and_raw_html() {
        let source = include_str!("../../../fixtures/plain/escaping.adoc");
        let parsed = parse(source).expect("valid source");

        assert_eq!(
            render(&parsed.ast, &HtmlOptions::default()).html,
            include_str!("../../../fixtures/plain/escaping.html")
        );
    }

    #[test]
    fn html_renderer_is_deterministic() {
        let parsed = parse("same input").expect("valid source");
        let options = HtmlOptions::default();

        assert_eq!(render(&parsed.ast, &options), render(&parsed.ast, &options));
    }

    #[test]
    fn html_renderer_can_wrap_a_complete_document() {
        let parsed = parse("paragraph").expect("valid source");

        assert_eq!(
            render(
                &parsed.ast,
                &HtmlOptions {
                    complete_document: true,
                    ..HtmlOptions::default()
                }
            )
            .html,
            concat!(
                "<!doctype html>\n",
                "<html>\n",
                "<body>\n",
                "<p>paragraph</p>\n",
                "</body>\n",
                "</html>\n"
            )
        );
    }

    #[test]
    fn heading_html_and_ids_match_fixture() {
        let source = include_str!("../../../fixtures/heading/basic.adoc");
        let parsed = parse(source).expect("valid source");
        let output = render(&parsed.ast, &HtmlOptions::default());

        assert_eq!(
            output.html,
            include_str!("../../../fixtures/heading/basic.html")
        );
        assert_eq!(
            output
                .heading_ids
                .iter()
                .map(|heading| heading.id.as_str())
                .collect::<Vec<_>>(),
            [
                "_document_title",
                "_hello_world",
                "_日本語",
                "_hello_world_2"
            ]
        );
    }

    #[test]
    fn heading_html_can_omit_document_title() {
        let parsed = parse("= Title\n\n== Section").expect("valid source");
        let output = render(
            &parsed.ast,
            &HtmlOptions {
                render_document_title: false,
                ..HtmlOptions::default()
            },
        );

        assert_eq!(output.html, "<h1 id=\"_section\">Section</h1>\n");
    }

    #[test]
    fn heading_id_has_a_deterministic_empty_fallback() {
        let parsed = parse("== !!!").expect("valid source");
        let output = render(&parsed.ast, &HtmlOptions::default());

        assert_eq!(output.heading_ids[0].id, "_section");
    }
}
