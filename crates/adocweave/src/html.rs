//! HTML output backend.
//!
//! This module depends on the output-neutral semantic AST. The parser and AST
//! do not depend on this module, so additional output backends can consume the
//! same document without changing parsing behavior.

use std::collections::BTreeMap;

use crate::attributes::AttributeOperation;
use crate::diagnostic::{Diagnostic, DiagnosticCode, DiagnosticId, Severity};
use crate::document::{HeadingId, ReferenceTarget, generate_heading_ids, reference_targets};
use crate::inline::{
    Inline, InlineLiteralKind, InlineStyle, Link, Reference, ReferenceDestination,
};
use crate::parser::{AstBlock, AstDocument, Heading, HeadingKind, Paragraph, Unsupported};
use crate::url::UrlPolicy;

pub const HTML_CONTRACT_VERSION: u16 = 2;
pub const ALLOWED_ELEMENTS: &[&str] = &[
    "a", "body", "code", "em", "h1", "h2", "h3", "h4", "h5", "html", "li", "ol", "p", "pre",
    "strong", "ul",
];
pub const ALLOWED_ATTRIBUTES: &[&str] = &["class", "href", "id"];
pub const ALLOWED_CLASSES: &[&str] = &["document-title", "language-*", "math-latex", "math-typst"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HtmlDocumentMode {
    Fragment,
    Complete,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderPolicy {
    pub document_mode: HtmlDocumentMode,
    pub render_document_title: bool,
    pub url_policy: UrlPolicy,
}

impl Default for RenderPolicy {
    fn default() -> Self {
        Self {
            document_mode: HtmlDocumentMode::Fragment,
            render_document_title: true,
            url_policy: UrlPolicy::default(),
        }
    }
}

impl RenderPolicy {
    pub fn allows_url(&self, value: &str) -> bool {
        self.url_policy.allows(value)
    }

    pub fn classify_url(&self, value: &str) -> crate::url::UrlDecision {
        self.url_policy.classify(value)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HtmlOutput {
    pub contract_version: u16,
    pub html: String,
    pub diagnostics: Vec<Diagnostic>,
    pub document_attributes: BTreeMap<String, String>,
    pub heading_ids: Vec<HeadingId>,
}

pub fn render(document: &AstDocument, policy: &RenderPolicy) -> HtmlOutput {
    render_with_resolutions(document, policy, &[])
}

pub use crate::reference::ResolvedReference;

pub fn render_with_resolutions(
    document: &AstDocument,
    policy: &RenderPolicy,
    resolutions: &[ResolvedReference],
) -> HtmlOutput {
    let mut fragment = String::new();
    let mut document_attributes = BTreeMap::new();
    for attribute in document.attributes() {
        match &attribute.operation {
            AttributeOperation::Set => {
                document_attributes.insert(attribute.name.clone(), attribute.raw_value.clone());
            }
            AttributeOperation::Unset => {
                document_attributes.remove(&attribute.name);
            }
        }
    }
    let heading_ids = generate_heading_ids(document);
    let targets = reference_targets(document);
    let mut diagnostics = Vec::new();
    let mut inline_context = InlineRenderContext {
        attributes: &document_attributes,
        policy,
        targets: &targets,
        resolutions,
        diagnostics: &mut diagnostics,
    };
    let mut heading_index = 0;
    for block in document.blocks() {
        let explicit_id = document
            .anchors()
            .iter()
            .find(|anchor| anchor.valid && anchor.target_range == Some(block.range()))
            .map(|anchor| anchor.id.as_str());
        match block {
            AstBlock::Heading(heading) => {
                let id = &heading_ids[heading_index].id;
                heading_index += 1;
                render_heading(&mut fragment, heading, id, policy, &mut inline_context);
            }
            AstBlock::Paragraph(paragraph) => {
                render_paragraph(&mut fragment, paragraph, explicit_id, &mut inline_context)
            }
            AstBlock::Literal(literal) => {
                fragment.push_str("<pre");
                render_optional_id(&mut fragment, explicit_id);
                fragment.push('>');
                escape_html_into(&mut fragment, &literal.value);
                fragment.push_str("</pre>\n");
            }
            AstBlock::Source(source) => {
                fragment.push_str("<pre");
                render_optional_id(&mut fragment, explicit_id);
                fragment.push_str("><code");
                if let Some(language) = &source.language {
                    fragment.push_str(" class=\"language-");
                    escape_html_into(&mut fragment, &safe_language_class(language));
                    fragment.push('"');
                }
                fragment.push('>');
                escape_html_into(&mut fragment, &source.value);
                fragment.push_str("</code></pre>\n");
            }
            AstBlock::List(list) => {
                render_list(&mut fragment, list, explicit_id, &mut inline_context)
            }
            AstBlock::Math(math) => {
                fragment.push_str("<pre");
                render_optional_id(&mut fragment, explicit_id);
                fragment.push_str(" class=\"");
                fragment.push_str(math_class(math.language));
                fragment.push_str("\"><code>");
                escape_html_into(&mut fragment, &math.value);
                fragment.push_str("</code></pre>\n");
            }
            AstBlock::Unsupported(unsupported) => {
                render_unsupported(&mut fragment, unsupported, explicit_id)
            }
        }
    }

    let html = if policy.document_mode == HtmlDocumentMode::Complete {
        format!("<!doctype html>\n<html>\n<body>\n{fragment}</body>\n</html>\n")
    } else {
        fragment
    };

    HtmlOutput {
        contract_version: HTML_CONTRACT_VERSION,
        html,
        diagnostics,
        document_attributes,
        heading_ids,
    }
}

fn render_list(
    output: &mut String,
    list: &crate::parser::ListBlock,
    explicit_id: Option<&str>,
    context: &mut InlineRenderContext<'_>,
) {
    let tag = match list.kind {
        crate::parser::ListKind::Unordered => "ul",
        crate::parser::ListKind::Ordered => "ol",
    };
    output.push('<');
    output.push_str(tag);
    render_optional_id(output, explicit_id);
    output.push_str(">\n");
    for item in &list.items {
        output.push_str("<li>");
        render_inlines(output, &item.inlines, context);
        for child in &item.children {
            output.push('\n');
            render_list(output, child, None, context);
        }
        for continuation in &item.continuations {
            match continuation {
                AstBlock::Literal(block) => {
                    output.push_str("\n<pre>");
                    escape_html_into(output, &block.value);
                    output.push_str("</pre>");
                }
                AstBlock::Source(block) => {
                    output.push_str("\n<pre><code");
                    if let Some(language) = &block.language {
                        output.push_str(" class=\"language-");
                        escape_html_into(output, &safe_language_class(language));
                        output.push('"');
                    }
                    output.push('>');
                    escape_html_into(output, &block.value);
                    output.push_str("</code></pre>");
                }
                _ => {}
            }
        }
        output.push_str("</li>\n");
    }
    output.push_str("</");
    output.push_str(tag);
    output.push_str(">\n");
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
    context: &mut InlineRenderContext<'_>,
) {
    if !heading.well_formed {
        output.push_str("<p>");
        render_inlines(output, &heading.inlines, context);
        output.push_str("</p>\n");
        return;
    }

    match heading.kind {
        HeadingKind::DocumentTitle if policy.render_document_title => {
            output.push_str("<h1 class=\"document-title\" id=\"");
            output.push_str(id);
            output.push_str("\">");
            render_inlines(output, &heading.inlines, context);
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
            render_inlines(output, &heading.inlines, context);
            output.push_str("</h");
            output.push(level);
            output.push_str(">\n");
        }
    }
}

fn render_paragraph(
    output: &mut String,
    paragraph: &Paragraph,
    id: Option<&str>,
    context: &mut InlineRenderContext<'_>,
) {
    output.push_str("<p");
    render_optional_id(output, id);
    output.push('>');
    render_inlines(output, &paragraph.inlines, context);
    output.push_str("</p>\n");
}

fn render_inlines(output: &mut String, inlines: &[Inline], context: &mut InlineRenderContext<'_>) {
    for inline in inlines {
        match inline {
            Inline::Text(text) => escape_inline_text(output, &text.value),
            Inline::Literal { kind, value, .. } => match kind {
                InlineLiteralKind::Monospace => {
                    output.push_str("<code>");
                    escape_inline_text(output, value);
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
                render_inlines(output, children, context);
                output.push_str("</");
                output.push_str(tag);
                output.push('>');
            }
            Inline::AttributeReference { name, .. } => {
                if let Some(value) = context.attributes.get(name) {
                    escape_html_into(output, value);
                } else {
                    output.push('{');
                    escape_html_into(output, name);
                    output.push('}');
                }
            }
            Inline::Link(link) => render_link(output, link, context),
            Inline::Reference(reference) => render_reference(output, reference, context),
            Inline::Formula(formula) => {
                output.push_str("<code class=\"");
                output.push_str(math_class(formula.language));
                output.push_str("\">");
                escape_inline_text(output, &formula.value);
                output.push_str("</code>");
            }
        }
    }
}

const fn math_class(language: crate::inline::MathLanguage) -> &'static str {
    match language {
        crate::inline::MathLanguage::Latex => "math-latex",
        crate::inline::MathLanguage::Typst => "math-typst",
    }
}

struct InlineRenderContext<'a> {
    attributes: &'a BTreeMap<String, String>,
    policy: &'a RenderPolicy,
    targets: &'a [ReferenceTarget],
    resolutions: &'a [ResolvedReference],
    diagnostics: &'a mut Vec<Diagnostic>,
}

fn render_link(output: &mut String, link: &Link, context: &mut InlineRenderContext<'_>) {
    if context.policy.allows_url(&link.target) {
        output.push_str("<a href=\"");
        escape_html_into(output, &link.target);
        output.push_str("\">");
        render_label_or_text(output, &link.label, &link.target_source, context);
        output.push_str("</a>");
    } else {
        render_label_or_text(output, &link.label, &link.target_source, context);
        context.diagnostics.push(render_diagnostic(
            "invalid-url-scheme",
            "URL is rejected by the render policy",
            link.target_range,
        ));
    }
}

fn render_reference(
    output: &mut String,
    reference: &Reference,
    context: &mut InlineRenderContext<'_>,
) {
    let (href, fallback, diagnostic) = match &reference.destination {
        ReferenceDestination::Local { anchor, .. } => {
            if let Some(target) = context.targets.iter().find(|target| target.id == *anchor) {
                (Some(format!("#{anchor}")), target.label.clone(), None)
            } else {
                (
                    None,
                    anchor.clone(),
                    Some(("unresolved-cross-reference", "local anchor does not exist")),
                )
            }
        }
        ReferenceDestination::Invalid => (
            None,
            reference_text(reference),
            Some(("invalid-cross-reference", "invalid cross reference target")),
        ),
        ReferenceDestination::Document { .. } | ReferenceDestination::Scheme { .. } => {
            let resolution = context
                .resolutions
                .iter()
                .find(|resolution| resolution.source_range == reference.range);
            if let Some(resolution) = resolution {
                match &resolution.outcome {
                    crate::reference::ResolutionOutcome::Resolved { href }
                        if context.policy.allows_url(href) =>
                    {
                        (Some(href.clone()), reference_text(reference), None)
                    }
                    crate::reference::ResolutionOutcome::Resolved { .. } => (
                        None,
                        reference_text(reference),
                        Some((
                            "invalid-url-scheme",
                            "resolved reference URL is rejected by the render policy",
                        )),
                    ),
                    crate::reference::ResolutionOutcome::Failed(failure) => (
                        None,
                        reference_text(reference),
                        Some((
                            failure.kind.diagnostic_code(),
                            "reference resolution failed",
                        )),
                    ),
                }
            } else {
                (
                    None,
                    reference_text(reference),
                    Some((
                        "unresolved-cross-reference",
                        "cross reference requires host resolution",
                    )),
                )
            }
        }
    };
    if let Some(href) = href {
        output.push_str("<a href=\"");
        escape_html_into(output, &href);
        output.push_str("\">");
        render_label_or_text(output, &reference.label, &fallback, context);
        output.push_str("</a>");
    } else {
        render_label_or_text(output, &reference.label, &fallback, context);
    }
    if let Some((code, message)) = diagnostic {
        context
            .diagnostics
            .push(render_diagnostic(code, message, reference.target_range));
    }
}

fn render_label_or_text(
    output: &mut String,
    label: &[Inline],
    fallback: &str,
    context: &mut InlineRenderContext<'_>,
) {
    if label.is_empty() {
        escape_html_into(output, fallback);
    } else {
        render_inlines(output, label, context);
    }
}

fn reference_text(reference: &Reference) -> String {
    reference.target_source.clone()
}

fn render_diagnostic(code: &str, message: &str, range: crate::source::TextRange) -> Diagnostic {
    Diagnostic {
        id: DiagnosticId::new(format!(
            "{code}@{}:{}",
            range.start().to_u32(),
            range.end().to_u32()
        )),
        code: DiagnosticCode::new(code),
        severity: Severity::Warning,
        message: message.to_owned(),
        range,
        related: Vec::new(),
        fixes: Vec::new(),
    }
}

fn render_unsupported(output: &mut String, unsupported: &Unsupported, id: Option<&str>) {
    output.push_str("<p");
    render_optional_id(output, id);
    output.push('>');
    escape_html_into(output, &unsupported.raw);
    output.push_str("</p>\n");
}

fn render_optional_id(output: &mut String, id: Option<&str>) {
    if let Some(id) = id {
        output.push_str(" id=\"");
        escape_html_into(output, id);
        output.push('"');
    }
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

fn escape_inline_text(output: &mut String, text: &str) {
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\r' {
            if characters.peek() == Some(&'\n') {
                characters.next();
            }
            output.push(' ');
        } else if character == '\n' {
            output.push(' ');
        } else {
            let mut encoded = [0; 4];
            escape_html_into(output, character.encode_utf8(&mut encoded));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ALLOWED_ATTRIBUTES, ALLOWED_CLASSES, ALLOWED_ELEMENTS, HTML_CONTRACT_VERSION,
        HtmlDocumentMode, RenderPolicy, ResolvedReference, render, render_with_resolutions,
    };
    use crate::inline::{Inline, ReferenceDestination};
    use crate::parser::AstBlock;
    use crate::parser::parse;
    use crate::url::UrlDecision;

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
    fn multiline_inline_spans_fold_source_endings_without_losing_markup() {
        let source =
            "before *strong\n日本語* and ``mono\r\ncode`` https://example.org[label\n続き]";
        let parsed = parse(source).expect("valid source");

        assert_eq!(
            render(&parsed.ast, &RenderPolicy::default()).html,
            "<p>before <strong>strong 日本語</strong> and <code>mono code</code> <a href=\"https://example.org\">label 続き</a></p>\n"
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

        policy
            .url_policy
            .allowed_schemes
            .insert("mailto".to_owned());
        policy.url_policy.allow_relative = true;
        assert!(policy.allows_url("mailto:user@example.com"));
        assert!(policy.allows_url("relative.adoc"));
        assert!(!policy.allows_url("../outside.adoc"));

        let parsed = parse("link:relative.adoc[relative]").expect("parse");
        assert_eq!(
            render(&parsed.ast, &policy).html,
            "<p><a href=\"relative.adoc\">relative</a></p>\n"
        );
    }

    #[test]
    fn html_contract_has_explicit_allowlists() {
        assert_eq!(HTML_CONTRACT_VERSION, 2);
        assert_eq!(
            ALLOWED_ELEMENTS,
            [
                "a", "body", "code", "em", "h1", "h2", "h3", "h4", "h5", "html", "li", "ol", "p",
                "pre", "strong", "ul"
            ]
        );
        assert_eq!(ALLOWED_ATTRIBUTES, ["class", "href", "id"]);
        assert_eq!(
            ALLOWED_CLASSES,
            ["document-title", "language-*", "math-latex", "math-typst"]
        );
        let parsed = parse("paragraph").expect("parse");
        assert_eq!(
            render(&parsed.ast, &RenderPolicy::default()).contract_version,
            HTML_CONTRACT_VERSION
        );
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
    fn links_apply_attributes_labels_and_url_policy() {
        let parsed = parse(
            "= Links\n:host: example.com\n\n\
             https://{host}[*safe*] javascript:alert(1)[unsafe]\n",
        )
        .expect("parse");
        let output = render(&parsed.ast, &RenderPolicy::default());

        assert!(
            output
                .html
                .contains("<a href=\"https://example.com\"><strong>safe</strong></a>")
        );
        assert!(output.html.contains(" unsafe</p>"));
        assert!(!output.html.contains("javascript:"));
        assert!(
            output
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code.as_str() == "invalid-url-scheme")
        );
    }

    #[test]
    fn link_target_attributes_expand_exactly_once() {
        let parsed = parse("= Links\n:a: {b}\n:b: expanded\n\nhttps://example.com/{a}[target]\n")
            .expect("parse");
        let AstBlock::Paragraph(paragraph) = &parsed.ast.blocks()[1] else {
            panic!("paragraph");
        };
        let Inline::Link(link) = &paragraph.inlines[0] else {
            panic!("link");
        };

        assert_eq!(link.target, "https://example.com/{b}");
        assert_ne!(link.target, "https://example.com/expanded");
    }

    #[test]
    fn cross_references_resolve_locally_or_from_safe_host_results() {
        let source = "[[local]]\n== Local\n\n<<local,Here>> xref:other.adoc#part[There]";
        let parsed = parse(source).expect("parse");
        let external = parsed
            .ast
            .blocks()
            .iter()
            .find_map(|block| match block {
                AstBlock::Paragraph(paragraph) => {
                    paragraph.inlines.iter().find_map(|inline| match inline {
                        Inline::Reference(reference)
                            if matches!(
                                reference.destination,
                                ReferenceDestination::Document { .. }
                            ) =>
                        {
                            Some(reference.range)
                        }
                        _ => None,
                    })
                }
                _ => None,
            })
            .expect("external reference");
        let output = render_with_resolutions(
            &parsed.ast,
            &RenderPolicy::default(),
            &[ResolvedReference::resolved(
                external,
                "https://notes.example/part",
            )],
        );

        assert!(output.html.contains("<a href=\"#local\">Here</a>"));
        assert!(
            output
                .html
                .contains("<a href=\"https://notes.example/part\">There</a>")
        );
        assert!(output.diagnostics.is_empty());
    }

    #[test]
    fn unresolved_cross_references_render_as_safe_non_links() {
        let parsed = parse("xref:#missing[<Missing>] xref:other.adoc[Other]").expect("parse");
        let output = render(&parsed.ast, &RenderPolicy::default());

        assert_eq!(output.html, "<p>&lt;Missing&gt; Other</p>\n");
        assert_eq!(
            output
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code.as_str())
                .collect::<Vec<_>>(),
            ["unresolved-cross-reference", "unresolved-cross-reference"]
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

    #[test]
    fn anchors_use_the_same_ids_in_html_and_reference_index() {
        let parsed =
            parse("[[heading-id]]\n== Heading\n\n[#paragraph-id]\nParagraph\n").expect("parse");
        let output = render(&parsed.ast, &RenderPolicy::default());

        assert_eq!(
            output.html,
            "<h1 id=\"heading-id\">Heading</h1>\n\
             <p id=\"paragraph-id\">Paragraph</p>\n"
        );
        let target_ids = crate::document::reference_targets(&parsed.ast)
            .into_iter()
            .map(|target| target.id)
            .collect::<Vec<_>>();
        assert_eq!(target_ids, ["heading-id", "paragraph-id"]);
    }

    #[test]
    fn lists_render_nested_and_continued_blocks() {
        let parsed = parse("* one\n** nested\n* code\n+\n....\n<raw>\n....\n").expect("parse");
        let output = render(&parsed.ast, &RenderPolicy::default());

        assert!(output.html.contains("<li>one\n<ul>"));
        assert!(output.html.contains("<pre>&lt;raw&gt;"));
    }

    #[test]
    fn lists_match_the_supported_asciidoctor_fixture() {
        let parsed = parse(include_str!(
            "../../../fixtures/lists/asciidoctor-compatible.adoc"
        ))
        .expect("parse");
        let output = render(&parsed.ast, &RenderPolicy::default());

        assert_eq!(
            output.html,
            include_str!("../../../fixtures/lists/asciidoctor-compatible.html")
        );
    }

    #[test]
    fn reference_fallback_preserves_the_source_scheme_spelling() {
        let parsed = parse("xref:Note:123[]").expect("parse");
        let output = render(&parsed.ast, &RenderPolicy::default());

        assert!(output.html.contains("Note:123"));
    }

    #[test]
    fn stem_html_is_escaped_and_matches_the_substitution_fixture() {
        let parsed =
            parse(include_str!("../../../fixtures/stem/substitutions.adoc")).expect("parse");
        let output = render(&parsed.ast, &RenderPolicy::default());

        assert_eq!(
            output.html,
            include_str!("../../../fixtures/stem/substitutions.html")
        );
        assert!(!output.html.contains("<z>"));
    }
}
