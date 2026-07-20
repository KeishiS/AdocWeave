//! HTML output backend.
//!
//! This module depends on the output-neutral semantic AST. The parser and AST
//! do not depend on this module, so additional output backends can consume the
//! same document without changing parsing behavior.

use std::collections::{BTreeMap, BTreeSet};

use crate::attributes::AttributeOperation;
use crate::diagnostic::Diagnostic;
use crate::document::{HeadingId, generate_heading_ids};
use crate::inline::{Inline, InlineLiteralKind, InlineStyle};
use crate::parser::{AstBlock, AstDocument, Heading, HeadingKind, Paragraph, Unsupported};

pub const HTML_CONTRACT_VERSION: u16 = 1;
pub const ALLOWED_ELEMENTS: &[&str] = &[
    "body", "code", "em", "h1", "h2", "h3", "h4", "h5", "html", "p", "pre", "strong",
];
pub const ALLOWED_ATTRIBUTES: &[&str] = &["class", "href", "id"];
pub const ALLOWED_CLASSES: &[&str] = &["document-title", "language-*"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HtmlDocumentMode {
    Fragment,
    Complete,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderPolicy {
    pub contract_version: u16,
    pub document_mode: HtmlDocumentMode,
    pub render_document_title: bool,
    pub allowed_url_schemes: BTreeSet<String>,
    pub allow_relative_urls: bool,
    pub allow_external_images: bool,
    pub allow_data_uris: bool,
}

impl Default for RenderPolicy {
    fn default() -> Self {
        Self {
            contract_version: HTML_CONTRACT_VERSION,
            document_mode: HtmlDocumentMode::Fragment,
            render_document_title: true,
            allowed_url_schemes: ["http", "https"].map(String::from).into_iter().collect(),
            allow_relative_urls: false,
            allow_external_images: false,
            allow_data_uris: false,
        }
    }
}

impl RenderPolicy {
    pub fn allows_url(&self, value: &str) -> bool {
        matches!(self.classify_url(value), UrlDecision::Allowed)
    }

    pub fn classify_url(&self, value: &str) -> UrlDecision {
        if value.is_empty()
            || value
                .chars()
                .any(|character| character.is_control() || character.is_whitespace())
            || contains_encoded_control(value)
        {
            return UrlDecision::Rejected;
        }
        let Some(colon) = value.find(':') else {
            return if self.allow_relative_urls
                && !value.starts_with('/')
                && !value.starts_with('\\')
                && !value.contains('\\')
                && !value.split('/').any(|segment| segment == "..")
            {
                UrlDecision::Allowed
            } else {
                UrlDecision::Rejected
            };
        };
        let scheme = &value[..colon];
        if scheme.is_empty()
            || !scheme.bytes().enumerate().all(|(index, byte)| {
                byte.is_ascii_alphanumeric() || (index > 0 && matches!(byte, b'+' | b'-' | b'.'))
            })
            || !scheme.as_bytes()[0].is_ascii_alphabetic()
        {
            return UrlDecision::Rejected;
        }
        let normalized = scheme.to_ascii_lowercase();
        if normalized == "data" && !self.allow_data_uris {
            return UrlDecision::Rejected;
        }
        if self.allowed_url_schemes.contains(&normalized) {
            UrlDecision::Allowed
        } else {
            UrlDecision::Rejected
        }
    }
}

fn contains_encoded_control(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.windows(3).any(|window| {
        if window[0] != b'%' {
            return false;
        }
        let Some(high) = hex(window[1]) else {
            return false;
        };
        let Some(low) = hex(window[2]) else {
            return false;
        };
        let decoded = high * 16 + low;
        decoded <= 0x20 || decoded == 0x7f
    })
}

const fn hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UrlDecision {
    Allowed,
    Rejected,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HtmlOutput {
    pub html: String,
    pub diagnostics: Vec<Diagnostic>,
    pub document_attributes: BTreeMap<String, String>,
    pub heading_ids: Vec<HeadingId>,
}

pub fn render(document: &AstDocument, policy: &RenderPolicy) -> HtmlOutput {
    let mut fragment = String::new();
    let mut document_attributes = BTreeMap::new();
    for attribute in &document.attributes {
        match &attribute.operation {
            AttributeOperation::Set(_) => {
                document_attributes.insert(attribute.name.clone(), attribute.raw_value.clone());
            }
            AttributeOperation::Unset => {
                document_attributes.remove(&attribute.name);
            }
        }
    }
    let heading_ids = generate_heading_ids(document);
    let mut heading_index = 0;
    for block in &document.blocks {
        match block {
            AstBlock::Heading(heading) => {
                let id = &heading_ids[heading_index].id;
                heading_index += 1;
                render_heading(&mut fragment, heading, id, policy, &document_attributes);
            }
            AstBlock::Paragraph(paragraph) => {
                render_paragraph(&mut fragment, paragraph, &document_attributes)
            }
            AstBlock::Literal(literal) => {
                fragment.push_str("<pre>");
                escape_html_into(&mut fragment, &literal.value);
                fragment.push_str("</pre>\n");
            }
            AstBlock::Source(source) => {
                fragment.push_str("<pre><code");
                if let Some(language) = &source.language {
                    fragment.push_str(" class=\"language-");
                    escape_html_into(&mut fragment, &safe_language_class(language));
                    fragment.push('"');
                }
                fragment.push('>');
                escape_html_into(&mut fragment, &source.value);
                fragment.push_str("</code></pre>\n");
            }
            AstBlock::Unsupported(unsupported) => render_unsupported(&mut fragment, unsupported),
        }
    }

    let html = if policy.document_mode == HtmlDocumentMode::Complete {
        format!("<!doctype html>\n<html>\n<body>\n{fragment}</body>\n</html>\n")
    } else {
        fragment
    };

    HtmlOutput {
        html,
        diagnostics: Vec::new(),
        document_attributes,
        heading_ids,
    }
}

fn safe_language_class(language: &str) -> String {
    language
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

fn render_heading(
    output: &mut String,
    heading: &Heading,
    id: &str,
    policy: &RenderPolicy,
    attributes: &BTreeMap<String, String>,
) {
    if !heading.problems.is_empty() {
        output.push_str("<p>");
        render_inlines(output, &heading.inlines, attributes);
        output.push_str("</p>\n");
        return;
    }

    match heading.kind {
        HeadingKind::DocumentTitle if policy.render_document_title => {
            output.push_str("<h1 class=\"document-title\" id=\"");
            output.push_str(id);
            output.push_str("\">");
            render_inlines(output, &heading.inlines, attributes);
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
            render_inlines(output, &heading.inlines, attributes);
            output.push_str("</h");
            output.push(level);
            output.push_str(">\n");
        }
    }
}

fn render_paragraph(
    output: &mut String,
    paragraph: &Paragraph,
    attributes: &BTreeMap<String, String>,
) {
    output.push_str("<p>");
    for (index, line) in paragraph.lines.iter().enumerate() {
        if index != 0 {
            output.push(' ');
        }
        render_inlines(output, &line.inlines, attributes);
    }
    output.push_str("</p>\n");
}

fn render_inlines(output: &mut String, inlines: &[Inline], attributes: &BTreeMap<String, String>) {
    for inline in inlines {
        match inline {
            Inline::Text(text) => escape_html_into(output, &text.value),
            Inline::Literal { kind, value, .. } => match kind {
                InlineLiteralKind::Monospace => {
                    output.push_str("<code>");
                    escape_html_into(output, value);
                    output.push_str("</code>");
                }
            },
            Inline::Styled {
                style, children, ..
            } => {
                let tag = match style {
                    InlineStyle::Strong => "strong",
                    InlineStyle::Emphasis => "em",
                };
                output.push('<');
                output.push_str(tag);
                output.push('>');
                render_inlines(output, children, attributes);
                output.push_str("</");
                output.push_str(tag);
                output.push('>');
            }
            Inline::AttributeReference { name, .. } => {
                if let Some(value) = attributes.get(name) {
                    escape_html_into(output, value);
                } else {
                    output.push('{');
                    escape_html_into(output, name);
                    output.push('}');
                }
            }
        }
    }
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
    use super::{
        ALLOWED_ATTRIBUTES, ALLOWED_CLASSES, ALLOWED_ELEMENTS, HTML_CONTRACT_VERSION,
        HtmlDocumentMode, RenderPolicy, UrlDecision, render,
    };
    use crate::parser::parse;

    #[test]
    fn html_renderer_renders_paragraphs_and_folds_source_lines() {
        let parsed = parse("first line\nsecond line\n\nlast").expect("valid source");

        assert_eq!(
            render(&parsed.ast, &RenderPolicy::default()).html,
            "<p>first line second line</p>\n<p>last</p>\n"
        );
    }

    #[test]
    fn inline_regression_keeps_plain_text_html_output_unchanged() {
        let parsed = parse("plain <text>\nnext").expect("valid source");

        assert_eq!(
            render(&parsed.ast, &RenderPolicy::default()).html,
            "<p>plain &lt;text&gt; next</p>\n"
        );
    }

    #[test]
    fn monospace_html_escapes_code_content() {
        let parsed = parse("use `<tag>` now").expect("valid source");

        assert_eq!(
            render(&parsed.ast, &RenderPolicy::default()).html,
            "<p>use <code>&lt;tag&gt;</code> now</p>\n"
        );
    }

    #[test]
    fn strong_html_renders_nested_inlines() {
        let parsed = parse("*bold and `code`*").expect("valid source");

        assert_eq!(
            render(&parsed.ast, &RenderPolicy::default()).html,
            "<p><strong>bold and <code>code</code></strong></p>\n"
        );
    }

    #[test]
    fn emphasis_html_renders_nested_inlines() {
        let parsed = parse("_italic and *bold*_").expect("valid source");

        assert_eq!(
            render(&parsed.ast, &RenderPolicy::default()).html,
            "<p><em>italic and <strong>bold</strong></em></p>\n"
        );
    }

    #[test]
    fn literal_block_html_escapes_content_without_inline_parsing() {
        let parsed = parse("....\n<tag> & *strong*\n....\n").expect("valid source");

        assert_eq!(
            render(&parsed.ast, &RenderPolicy::default()).html,
            "<pre>&lt;tag&gt; &amp; *strong*\n</pre>\n"
        );
    }

    #[test]
    fn source_block_html_escapes_code_and_sanitizes_language_class() {
        let parsed = parse("[source, Rust<script>]\n----\n<&>\n----\n").expect("valid source");

        assert_eq!(
            render(&parsed.ast, &RenderPolicy::default()).html,
            "<pre><code class=\"language-rust-script-\">&lt;&amp;&gt;\n</code></pre>\n"
        );
    }

    #[test]
    fn html_renderer_escapes_all_special_characters_and_raw_html() {
        let source = include_str!("../../../fixtures/plain/escaping.adoc");
        let parsed = parse(source).expect("valid source");

        assert_eq!(
            render(&parsed.ast, &RenderPolicy::default()).html,
            include_str!("../../../fixtures/plain/escaping.html")
        );
    }

    #[test]
    fn html_renderer_is_deterministic() {
        let parsed = parse("same input").expect("valid source");
        let options = RenderPolicy::default();

        assert_eq!(render(&parsed.ast, &options), render(&parsed.ast, &options));
    }

    #[test]
    fn html_renderer_can_wrap_a_complete_document() {
        let parsed = parse("paragraph").expect("valid source");

        assert_eq!(
            render(
                &parsed.ast,
                &RenderPolicy {
                    document_mode: HtmlDocumentMode::Complete,
                    ..RenderPolicy::default()
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
    fn html_contract_golden_covers_fragment_and_complete_document() {
        let parsed =
            parse(include_str!("../../../fixtures/html/contract.adoc")).expect("valid source");
        assert_eq!(
            render(&parsed.ast, &RenderPolicy::default()).html,
            include_str!("../../../fixtures/html/contract.fragment.html")
        );
        assert_eq!(
            render(
                &parsed.ast,
                &RenderPolicy {
                    document_mode: HtmlDocumentMode::Complete,
                    ..RenderPolicy::default()
                }
            )
            .html,
            include_str!("../../../fixtures/html/contract.complete.html")
        );
    }

    #[test]
    fn render_policy_allows_only_configured_safe_schemes() {
        let mut policy = RenderPolicy::default();
        assert_eq!(
            policy.classify_url("https://example.com"),
            UrlDecision::Allowed
        );
        assert_eq!(
            policy.classify_url("HTTP://example.com"),
            UrlDecision::Allowed
        );
        assert_eq!(
            policy.classify_url("javascript:alert(1)"),
            UrlDecision::Rejected
        );
        assert_eq!(
            policy.classify_url("java%0ascript:alert(1)"),
            UrlDecision::Rejected
        );
        assert_eq!(policy.classify_url("relative.adoc"), UrlDecision::Rejected);
        assert_eq!(policy.classify_url("/absolute"), UrlDecision::Rejected);
        assert_eq!(
            policy.classify_url("data:text/html,x"),
            UrlDecision::Rejected
        );

        policy.allowed_url_schemes.insert("mailto".to_owned());
        policy.allow_relative_urls = true;
        assert!(policy.allows_url("mailto:user@example.com"));
        assert!(policy.allows_url("relative.adoc"));
        assert!(!policy.allows_url("../outside.adoc"));
    }

    #[test]
    fn html_contract_has_explicit_allowlists() {
        assert_eq!(HTML_CONTRACT_VERSION, 1);
        assert_eq!(
            ALLOWED_ELEMENTS,
            [
                "body", "code", "em", "h1", "h2", "h3", "h4", "h5", "html", "p", "pre", "strong"
            ]
        );
        assert_eq!(ALLOWED_ATTRIBUTES, ["class", "href", "id"]);
        assert_eq!(ALLOWED_CLASSES, ["document-title", "language-*"]);
    }

    #[test]
    fn html_security_never_passes_input_elements_or_attributes_through() {
        let parsed = parse(
            "<script>alert(1)</script>\n\
             <svg onload=\"alert(1)\"></svg>\n\
             <p style=\"color:red\">unsafe</p>\n",
        )
        .expect("valid source");
        let html = render(&parsed.ast, &RenderPolicy::default()).html;

        assert!(!html.contains("<script"));
        assert!(!html.contains("<svg"));
        assert!(!html.contains("<svg onload="));
        assert!(!html.contains("<p style="));
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.contains("&lt;svg onload=&#34;alert(1)&#34;&gt;"));
    }

    #[test]
    fn document_attributes_are_substituted_once_and_exposed_as_metadata() {
        let parsed = parse("= Note\n:name: <Alice>\n\nHello {name}; {missing}.\n").expect("parse");
        let output = render(&parsed.ast, &RenderPolicy::default());

        assert_eq!(
            output.html,
            "<h1 class=\"document-title\" id=\"_note\">Note</h1>\n\
             <p>Hello &lt;Alice&gt;; {missing}.</p>\n"
        );
        assert_eq!(
            output.document_attributes.get("name"),
            Some(&"<Alice>".to_owned())
        );
    }

    #[test]
    fn heading_html_and_ids_match_fixture() {
        let source = include_str!("../../../fixtures/heading/basic.adoc");
        let parsed = parse(source).expect("valid source");
        let output = render(&parsed.ast, &RenderPolicy::default());

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
            &RenderPolicy {
                render_document_title: false,
                ..RenderPolicy::default()
            },
        );

        assert_eq!(output.html, "<h1 id=\"_section\">Section</h1>\n");
    }

    #[test]
    fn heading_id_has_a_deterministic_empty_fallback() {
        let parsed = parse("== !!!").expect("valid source");
        let output = render(&parsed.ast, &RenderPolicy::default());

        assert_eq!(output.heading_ids[0].id, "_section");
    }
}
