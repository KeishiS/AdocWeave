//! HTML output backend.
//!
//! This module depends on the output-neutral semantic AST. The parser and AST
//! do not depend on this module, so additional output backends can consume the
//! same document without changing parsing behavior.

use std::collections::BTreeMap;

use crate::diagnostic::Diagnostic;
use crate::parser::{AstBlock, AstDocument, Heading, Paragraph, Unsupported};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HtmlOptions {
    pub complete_document: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HtmlOutput {
    pub html: String,
    pub diagnostics: Vec<Diagnostic>,
    pub document_attributes: BTreeMap<String, String>,
}

pub fn render(document: &AstDocument, options: &HtmlOptions) -> HtmlOutput {
    let mut fragment = String::new();
    for block in &document.blocks {
        match block {
            AstBlock::Heading(heading) => render_heading_as_plain_text(&mut fragment, heading),
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
    }
}

fn render_heading_as_plain_text(output: &mut String, heading: &Heading) {
    output.push_str("<p>");
    escape_html_into(output, &heading.text);
    output.push_str("</p>\n");
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
}
