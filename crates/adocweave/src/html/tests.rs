use std::collections::BTreeSet;

use super::{
    ALLOWED_ATTRIBUTES, ALLOWED_CLASSES, ALLOWED_ELEMENTS, ExternalLinkPresentation,
    HtmlDocumentMode, MathLanguagePolicy, RenderPolicy, ResolvedReference, ResourceCapabilities,
    SourceLanguagePolicy, StylesheetPolicy, StylesheetSource, UnknownSourceLanguage,
    UnresolvedReferencePresentation,
};
use crate::block_model::AstBlock;
use crate::diagnostic::{Diagnostic, Severity};
use crate::inline_model::Inline;
use crate::parser::parse;
use crate::reference::ReferenceKey;
use crate::render::RenderInputs;
use crate::resolution::{GeneratedBibliography, GeneratedBibliographyEntry};
use crate::resource::{MediaType, ResolvedResource};
use crate::url::{UrlDecision, UrlProvenance};

fn render(document: &crate::block_model::AstDocument, policy: &RenderPolicy) -> super::HtmlOutput {
    super::render_with_inputs_ast(document, policy, &RenderInputs::default())
}

fn render_with_inputs(
    document: &crate::block_model::AstDocument,
    policy: &RenderPolicy,
    inputs: &RenderInputs,
) -> super::HtmlOutput {
    super::render_with_inputs_ast(document, policy, inputs)
}

fn analyze(source: &str) -> crate::core::Analysis {
    crate::core::Engine::new(crate::core::AnalysisOptions::default())
        .analyze(source)
        .expect("analysis")
}

fn echo_resource_inputs(document: &crate::block_model::AstDocument) -> RenderInputs {
    let mut resources = Vec::new();
    crate::walker::walk_ast(document, |node| {
        if let crate::walker::SemanticNode::Inline(Inline::Macro(node)) = node {
            for reference in crate::resource::ResourceReference::from_macro(node) {
                let media_type = match reference.purpose() {
                    crate::resource::ResourcePurpose::Image
                    | crate::resource::ResourcePurpose::Icon
                    | crate::resource::ResourcePurpose::VideoPoster => "image/png",
                    crate::resource::ResourcePurpose::Audio => "audio/ogg",
                    crate::resource::ResourcePurpose::Video => "video/mp4",
                };
                resources.push(ResolvedResource::resolved(
                    reference.range(),
                    reference.target(),
                    media_type.parse().expect("test media type"),
                    None,
                ));
            }
        }
    });
    RenderInputs::default().with_resources(resources)
}

#[test]
fn html_renderer_renders_paragraphs_and_folds_source_lines() {
    let parsed = parse("first line\nsecond line\n\nlast").expect("valid source");

    assert_eq!(
        render(&parsed.ast, &RenderPolicy::default()).html,
        "<p>first line second line</p>\n<p>last</p>\n"
    );
}

/// The specification turns a line break inside a paragraph into a space.
/// Between two characters of a script written without word spaces, that
/// space is one the sentence never asked for, so it is not written.
#[test]
fn wrapped_lines_join_without_a_space_between_cjk_characters() {
    let parsed = parse("これは日本語の段落です。\n行を折り返しています。").expect("valid source");

    assert_eq!(
        render(&parsed.ast, &RenderPolicy::default()).html,
        "<p>これは日本語の段落です。行を折り返しています。</p>\n"
    );
}

/// A wrap that meets Latin text keeps its space, because a space belongs
/// between a Latin word and its neighbour.
#[test]
fn wrapped_lines_keep_the_space_where_a_line_meets_latin_text() {
    let parsed = parse("日本語とEnglish\nmixed の場合。").expect("valid source");

    assert_eq!(
        render(&parsed.ast, &RenderPolicy::default()).html,
        "<p>日本語とEnglish mixed の場合。</p>\n"
    );
}

/// A formatting pair keeps the spaces beside it. The unconstrained form
/// removes the need for them, so a space there was chosen by the author
/// rather than demanded by the syntax.
#[test]
fn formatting_pairs_keep_the_spaces_the_author_wrote() {
    let parsed = parse("日本語は *強調* です。\n\n日本語は**強調**です。").expect("valid source");

    assert_eq!(
        render(&parsed.ast, &RenderPolicy::default()).html,
        "<p>日本語は <strong>強調</strong> です。</p>\n<p>日本語は<strong>強調</strong>です。</p>\n"
    );
}

#[test]
fn constrained_formatting_keeps_its_spaces_in_latin_text() {
    let parsed = parse("English *bold* here.").expect("valid source");

    assert_eq!(
        render(&parsed.ast, &RenderPolicy::default()).html,
        "<p>English <strong>bold</strong> here.</p>\n"
    );
}

#[test]
fn an_inline_macro_drops_the_space_that_let_it_be_written() {
    let parsed = parse("本文は link:https://example.com[ラベル] です。").expect("valid source");

    assert_eq!(
        render(&parsed.ast, &RenderPolicy::default()).html,
        "<p>本文は<a href=\"https://example.com\">ラベル</a> です。</p>\n"
    );
}

/// The label decides nothing. What is examined is the running text before
/// the space, so a Latin label loses the space just the same.
#[test]
fn a_latin_label_does_not_change_the_decision() {
    let parsed = parse("本文は link:https://example.com[Rust] です。").expect("valid source");

    assert_eq!(
        render(&parsed.ast, &RenderPolicy::default()).html,
        "<p>本文は<a href=\"https://example.com\">Rust</a> です。</p>\n"
    );
}

/// The space follows from the macro being written, not from what it turns
/// into. A macro the host renders as plain text demanded its space all the
/// same, so the result does not depend on how the host was configured.
#[test]
fn a_macro_the_host_renders_as_text_still_gives_back_its_space() {
    let parsed = parse("画像は image:a.png[図] です。").expect("valid source");

    assert_eq!(
        render(&parsed.ast, &RenderPolicy::default()).html,
        "<p>画像は図 です。</p>\n"
    );
}

#[test]
fn resolved_block_titles_render_inline_semantics_for_captions_and_admonitions() {
    let parsed = parse(
        "= Title\n:product: AdocWeave\n\n.The *bold* {product} caption\n|===\n|cell\n|===\n\n.*Important* {product}\n[NOTE]\n====\nbody\n====\n",
    )
    .expect("parse");

    assert_eq!(
        render(&parsed.ast, &RenderPolicy::default()).html,
        "<h1 class=\"document-title\" id=\"_title\">Title</h1>\n<table class=\"table-frame-all table-grid-all table-stripes-none\">\n<caption>The <strong>bold</strong> AdocWeave caption</caption>\n<tbody>\n<tr>\n<td class=\"table-align-left table-valign-top\">cell</td>\n</tr>\n</tbody>\n</table>\n<div class=\"admonition admonition-note\"><div class=\"title\"><strong>Important</strong> AdocWeave</div>\n<p>body</p>\n</div>\n"
    );
}

#[test]
fn appendix_class_comes_from_the_shared_document_structure() {
    let parsed = parse("= Book\n:doctype: book\n\n[appendix]\n== Reference\n").expect("parse");

    assert_eq!(
        render(&parsed.ast, &RenderPolicy::default()).html,
        "<h1 class=\"document-title\" id=\"_book\">Book</h1>\n<h1 class=\"appendix\" id=\"_reference\">Reference</h1>\n"
    );
}

#[test]
fn toc_and_section_numbers_render_from_document_presentation_layout() {
    let parsed = parse(
        "= Book\n:toc:\n:toclevels: 1\n:sectnums:\n\n== First\n=== Hidden child\n\n== Second\n",
    )
    .expect("parse");

    assert_eq!(
        render(&parsed.ast, &RenderPolicy::default()).html,
        "<h1 class=\"document-title\" id=\"_book\">Book</h1>\n<div class=\"toc\">\n<ul>\n<li><a href=\"#_first\">1. First</a></li>\n<li><a href=\"#_second\">2. Second</a></li>\n</ul>\n</div>\n<h1 id=\"_first\">1. First</h1>\n<h2 id=\"_hidden_child\">1.1. Hidden child</h2>\n<h1 id=\"_second\">2. Second</h1>\n"
    );
}

#[test]
fn book_parts_and_appendices_keep_presentation_numbers_without_changing_ids() {
    let parsed = parse(
        "= Book\n:doctype: book\n:toc:\n:sectnums:\n\n= Part\n\n== Chapter\n\n[appendix]\n== Reference\n",
    )
    .expect("parse");

    assert_eq!(
        render(&parsed.ast, &RenderPolicy::default()).html,
        "<h1 class=\"document-title\" id=\"_book\">Book</h1>\n<div class=\"toc\">\n<ul>\n<li><a href=\"#_part\">1. Part</a><ul>\n<li><a href=\"#_chapter\">1.1. Chapter</a></li>\n<li><a href=\"#_reference\">1.2. Reference</a></li>\n</ul>\n</li>\n</ul>\n</div>\n<h1 id=\"_part\">1. Part</h1>\n<h1 id=\"_chapter\">1.1. Chapter</h1>\n<h1 class=\"appendix\" id=\"_reference\">1.2. Reference</h1>\n"
    );
}

#[test]
fn cite_keeps_every_key_in_source_order_and_follows_the_unresolved_policy() {
    let source = "See cite:[smith2024, tanaka2025] and cite:[a, locator=\"p. 12\"].\n";
    let parsed = parse(source).expect("parse");
    let output = render(&parsed.ast, &RenderPolicy::default()).html;

    assert!(output.contains("<span class=\"citation\">smith2024</span>"));
    assert!(output.contains("<span class=\"citation\">tanaka2025</span>"));
    assert!(
        output.find("smith2024").expect("first key")
            < output.find("tanaka2025").expect("second key")
    );
    // A named attribute describes the citation and is not a key.
    assert!(!output.contains("p. 12"));

    let hidden = render(
        &parsed.ast,
        &RenderPolicy {
            unresolved_references: UnresolvedReferencePresentation::Hidden,
            ..RenderPolicy::default()
        },
    )
    .html;
    assert!(!hidden.contains("citation"));
}

#[test]
fn a_host_resolved_citation_replaces_the_keys_with_the_supplied_text() {
    let source = "= References\n\n[bibliography]\n== Sources\n\n* bibanchor:smith2024[] Entry\n\nSee cite:[smith2024, tanaka2025].\n";
    let analysis = analyze(source);
    let citation = &analysis.citations()[0];
    let inputs =
        RenderInputs::default().with_citations(vec![crate::citation::ResolvedCitation::resolved(
            citation.range,
            vec![
                crate::citation::CitationSegment::text("("),
                crate::citation::CitationSegment::linked("Smith 2024", "smith2024"),
                crate::citation::CitationSegment::text("; Tanaka 2025)"),
            ],
        )]);
    let output = render_with_inputs(analysis.ast(), &RenderPolicy::default(), &inputs);

    assert!(
        output
            .html
            .contains("(<a href=\"#smith2024\">Smith 2024</a>; Tanaka 2025)</span>")
    );
    // The host's text replaces the keys rather than joining them.
    assert!(!output.html.contains("tanaka2025"));
    assert!(output.diagnostics.is_empty());

    // The entry links back to the citation, so the landing point must exist
    // even though the host's text replaced the key that carried it.
    let backref = output
        .html
        .split("class=\"bibliography-backref\" href=\"#")
        .nth(1)
        .expect("back reference")
        .split('"')
        .next()
        .expect("back reference target");
    assert!(output.html.contains(&format!("<span id=\"{backref}\">")));
}

#[test]
fn a_resolved_citation_is_data_and_never_markup_or_a_dead_link() {
    let source = "See cite:[smith2024].\n";
    let analysis = analyze(source);
    let citation = &analysis.citations()[0];
    let inputs =
        RenderInputs::default().with_citations(vec![crate::citation::ResolvedCitation::resolved(
            citation.range,
            vec![crate::citation::CitationSegment::linked(
                "<b>Smith</b> & Co",
                "never_defined",
            )],
        )]);
    let output = render_with_inputs(analysis.ast(), &RenderPolicy::default(), &inputs);

    // The supplied text is escaped, and the unknown anchor leaves plain text
    // behind instead of a link that goes nowhere.
    assert!(
        output
            .html
            .contains("<span class=\"citation\">&lt;b&gt;Smith&lt;/b&gt; &amp; Co</span>")
    );
    assert!(!output.html.contains("href=\"#never_defined\""));
    assert_eq!(
        output
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect::<Vec<_>>(),
        ["unknown-citation-anchor"]
    );
}

#[test]
fn generated_bibliography_is_plain_text_and_links_both_directions() {
    let analysis = analyze("See cite:[cpp] and cite:[cpp].\n");
    let inputs =
        RenderInputs::default().with_generated_bibliography(GeneratedBibliography::new(
            "References\nfrom library",
            vec![GeneratedBibliographyEntry::new(
                "cpp",
                "Effective C++ and More Effective C++; <b>& {author} pass:[x] +x+ ++x++ +++x+++",
            )
            .with_label("C++")],
        ));

    let output = render_with_inputs(analysis.ast(), &RenderPolicy::default(), &inputs);

    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    assert_eq!(output.html.matches("href=\"#cpp\">C++</a>").count(), 2);
    assert!(output.html.contains(
        "Effective C++ and More Effective C++; &lt;b&gt;&amp; {author} pass:[x] +x+ ++x++ +++x+++"
    ));
    assert!(!output.html.contains("<b>"));
    assert!(output.html.contains("<h2>References from library</h2>"));
    assert_eq!(
        output
            .html
            .matches("class=\"bibliography-backref\"")
            .count(),
        2
    );
    for target in output
        .html
        .split("class=\"bibliography-backref\" href=\"#")
        .skip(1)
        .map(|suffix| suffix.split('"').next().expect("back reference target"))
    {
        assert!(output.html.contains(&format!("id=\"{target}\"")));
    }
}

#[test]
fn resolved_citation_can_link_to_a_generated_bibliography_entry() {
    let analysis = analyze("See cite:[smith2024].\n");
    let citation = &analysis.citations()[0];
    let inputs = RenderInputs::default()
        .with_citations(vec![crate::citation::ResolvedCitation::resolved(
            citation.range,
            vec![crate::citation::CitationSegment::linked(
                "Smith (2024)",
                "smith2024",
            )],
        )])
        .with_generated_bibliography(GeneratedBibliography::new(
            "References",
            vec![GeneratedBibliographyEntry::new("smith2024", "Smith. Book.")],
        ));

    let output = render_with_inputs(analysis.ast(), &RenderPolicy::default(), &inputs);

    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    assert!(
        output
            .html
            .contains("<a href=\"#smith2024\">Smith (2024)</a>")
    );
    assert!(
        output
            .html
            .contains("<span id=\"smith2024\" class=\"bibliography-anchor\"></span>Smith. Book.")
    );
}

#[test]
fn generated_bibliography_reports_invalid_duplicate_shadowed_and_unused_entries() {
    let analysis = analyze(
        "[bibliography]\n== Sources\n\n* bibanchor:local[] Local\n\nSee cite:[used, local].\n",
    );
    let inputs = RenderInputs::default().with_generated_bibliography(GeneratedBibliography::new(
        "References",
        vec![
            GeneratedBibliographyEntry::new("", "invalid"),
            GeneratedBibliographyEntry::new("used", "used"),
            GeneratedBibliographyEntry::new("used", "duplicate"),
            GeneratedBibliographyEntry::new("local", "shadowed"),
            GeneratedBibliographyEntry::new("unused", "unused"),
        ],
    ));

    let output = render_with_inputs(analysis.ast(), &RenderPolicy::default(), &inputs);
    let codes = output
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.as_str())
        .collect::<BTreeSet<_>>();

    assert_eq!(
        codes,
        BTreeSet::from([
            "duplicate-generated-bibliography-entry",
            "invalid-generated-bibliography-entry",
            "shadowed-generated-bibliography-entry",
            "unused-generated-bibliography-entry",
        ])
    );
    assert_eq!(output.html.matches("id=\"used\"").count(), 1);
    assert_eq!(output.html.matches("id=\"local\"").count(), 1);
    assert!(!output.html.contains("duplicate"));
    assert!(!output.html.contains("shadowed"));
}

fn numbered_bibliography_render(numbers: &[Option<u32>]) -> super::HtmlOutput {
    let analysis = analyze("See cite:[one], cite:[two] and cite:[three].\n");
    let keys = ["one", "two", "three"];
    let entries = keys
        .iter()
        .zip(numbers)
        .map(|(key, number)| {
            let entry = GeneratedBibliographyEntry::new(*key, format!("Entry {key}."));
            match number {
                Some(number) => entry.with_number(*number),
                None => entry,
            }
        })
        .collect();
    let inputs = RenderInputs::default()
        .with_generated_bibliography(GeneratedBibliography::new("References", entries));

    render_with_inputs(analysis.ast(), &RenderPolicy::default(), &inputs)
}

fn numbering_diagnostic(output: &super::HtmlOutput) -> &Diagnostic {
    output
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code.as_str() == "invalid-generated-bibliography-numbering")
        .unwrap_or_else(|| panic!("numbering diagnostic, got {:?}", output.diagnostics))
}

#[test]
fn a_bibliography_numbered_from_one_becomes_an_ordered_list() {
    let output = numbered_bibliography_render(&[Some(1), Some(2), Some(3)]);

    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    assert!(output.html.contains("<ol>"));
    assert!(output.html.contains("</ol>"));
    assert!(!output.html.contains("<ul>"));
}

#[test]
fn a_bibliography_without_numbers_stays_an_unordered_list() {
    let output = numbered_bibliography_render(&[None, None, None]);

    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    assert!(output.html.contains("<ul>"));
    assert!(!output.html.contains("<ol>"));
}

#[test]
fn numbering_only_some_entries_leaves_the_bibliography_unrendered() {
    let output = numbered_bibliography_render(&[Some(1), None, Some(3)]);

    let diagnostic = numbering_diagnostic(&output);
    assert_eq!(diagnostic.severity, Severity::Error);
    assert!(diagnostic.message.contains("only 2 of its 3 entries"));
    assert!(!output.html.contains("References"));
}

#[test]
fn a_repeated_number_leaves_the_bibliography_unrendered() {
    let output = numbered_bibliography_render(&[Some(1), Some(1), Some(3)]);

    let diagnostic = numbering_diagnostic(&output);
    assert_eq!(diagnostic.severity, Severity::Error);
    assert!(diagnostic.message.contains("`two` is numbered 1 where 2"));
    assert!(!output.html.contains("References"));
}

#[test]
fn numbers_that_do_not_start_at_one_leave_the_bibliography_unrendered() {
    let output = numbered_bibliography_render(&[Some(4), Some(5), Some(6)]);

    let diagnostic = numbering_diagnostic(&output);
    assert!(diagnostic.message.contains("`one` is numbered 4 where 1"));
    assert!(!output.html.contains("References"));
}

#[test]
fn a_gap_left_by_a_dropped_entry_leaves_the_bibliography_unrendered() {
    let analysis = analyze(
        "[bibliography]\n== Sources\n\n* bibanchor:local[] Local\n\nSee cite:[one, local, three].\n",
    );
    let inputs = RenderInputs::default().with_generated_bibliography(GeneratedBibliography::new(
        "References",
        vec![
            GeneratedBibliographyEntry::new("one", "First.").with_number(1),
            GeneratedBibliographyEntry::new("local", "Shadowed.").with_number(2),
            GeneratedBibliographyEntry::new("three", "Third.").with_number(3),
        ],
    ));

    let output = render_with_inputs(analysis.ast(), &RenderPolicy::default(), &inputs);

    let diagnostic = numbering_diagnostic(&output);
    assert_eq!(diagnostic.severity, Severity::Error);
    assert!(diagnostic.message.contains("`three` is numbered 3 where 2"));
    assert!(!output.html.contains("References"));
}

#[test]
fn a_citation_resolution_that_matches_nothing_is_reported_as_an_unused_input() {
    let analysis = analyze("See cite:[smith2024].\n");
    let elsewhere = analysis.references().first().map_or_else(
        || analysis.citations()[0].keys[0].range,
        |reference| reference.range,
    );
    let inputs =
        RenderInputs::default().with_citations(vec![crate::citation::ResolvedCitation::resolved(
            elsewhere,
            vec![crate::citation::CitationSegment::text("(Smith 2024)")],
        )]);
    let output = render_with_inputs(analysis.ast(), &RenderPolicy::default(), &inputs);

    assert!(
        output
            .diagnostics
            .iter()
            .any(
                |diagnostic| diagnostic.code.as_str() == "unused-render-input"
                    && diagnostic.message.contains("citation")
            )
    );
    // Without a matching resolution the key follows the unresolved policy.
    assert!(
        output
            .html
            .contains("<span class=\"citation\">smith2024</span>")
    );
}

#[test]
fn cite_links_keys_defined_by_this_document_and_earns_a_back_reference() {
    let parsed = parse(
        "= References\n\n[bibliography]\n== Sources\n\n* bibanchor:ref[] Entry\n\nSee cite:[ref, absent].\n",
    )
    .expect("parse");
    let output = render(&parsed.ast, &RenderPolicy::default()).html;

    // The document defines `ref`, so the citation links to the entry and the
    // entry links back.
    assert!(output.contains("<a class=\"citation\" id=\"_bibliography_ref_"));
    assert!(output.contains("href=\"#ref\">ref</a>"));
    assert_eq!(output.matches("class=\"bibliography-backref\"").count(), 1);
    // `absent` belongs to a library outside the document and stays unresolved.
    assert!(output.contains("<span class=\"citation\">absent</span>"));
}

#[test]
fn cite_and_cross_reference_back_references_are_numbered_in_source_order() {
    let parsed = parse(
        "[bibliography]\n== Sources\n\n* bibanchor:ref[] Entry\n\nFirst cite:[ref] then <<ref>> then cite:[ref].\n",
    )
    .expect("parse");
    let output = render(&parsed.ast, &RenderPolicy::default()).html;

    // Two citations and one cross reference cite the same entry.
    assert_eq!(output.matches("class=\"bibliography-backref\"").count(), 3);
    // Back reference targets follow the order the reader meets the citing
    // sites, regardless of which pass collected them.
    let targets = output
        .match_indices("href=\"#_bibliography_ref_")
        .map(|(index, _)| {
            output[index..]
                .split('"')
                .nth(1)
                .expect("href value")
                .trim_start_matches("#_bibliography_ref_")
                .parse::<u32>()
                .expect("generated offset")
        })
        .collect::<Vec<_>>();
    let mut sorted = targets.clone();
    sorted.sort_unstable();
    assert_eq!(targets, sorted);
}

#[test]
fn bibliography_section_uses_catalog_entries_for_citation_back_references() {
    let parsed = parse(
        "= References\n\n[bibliography]\n== Sources\n\n* bibanchor:ref[] Entry\n\nSee <<ref,Entry>>.\n",
    )
    .expect("parse");
    let output = render(&parsed.ast, &RenderPolicy::default()).html;

    assert!(output.contains("<span id=\"ref\" class=\"bibliography-anchor\"></span>"));
    assert!(output.contains("class=\"bibliography-backref\""));
    assert!(output.contains("id=\"_bibliography_ref_"));
    assert!(output.contains("href=\"#_bibliography_ref_"));
}

#[test]
fn bibliography_back_references_are_scoped_to_bibliography_sections() {
    let parsed = parse(
        "* bibanchor:outside[] Outside\n\nSee <<outside>>.\n\n[bibliography]\n== Sources\n\n* bibanchor:inside[] Inside\n\nSee <<inside>>.\n\n== After\n\n* bibanchor:after[] After\n\nSee <<after>>.\n",
    )
    .expect("parse");
    let output = render(&parsed.ast, &RenderPolicy::default()).html;

    assert_eq!(output.matches("class=\"bibliography-backref\"").count(), 1);
    assert!(output.contains("href=\"#_bibliography_ref_"));
}

#[test]
fn bibliography_scope_survives_child_sections() {
    let parsed = parse(
        "= References\n\n[bibliography]\n== Sources\n\n=== Primary\n\n* bibanchor:ref[] Entry\n\nSee <<ref>>.\n\n== After\n",
    )
    .expect("parse");
    let output = render(&parsed.ast, &RenderPolicy::default()).html;

    assert_eq!(output.matches("class=\"bibliography-backref\"").count(), 1);
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
    let source = "before *strong\n日本語* and ``mono\r\ncode`` https://example.org[label\n続き]";
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
fn source_block_presentation_matches_the_shared_fixture() {
    let source = include_str!("../../../../fixtures/conformance/source-block-presentation.adoc");
    let expected = include_str!("../../../../fixtures/conformance/source-block-presentation.html");
    let parsed = parse(source).expect("valid source");

    assert_eq!(render(&parsed.ast, &RenderPolicy::default()).html, expected);
}

#[test]
fn html_renderer_escapes_all_special_characters_and_raw_html() {
    let source = include_str!("../../../../fixtures/plain/escaping.adoc");
    let parsed = parse(source).expect("valid source");

    assert_eq!(
        render(&parsed.ast, &RenderPolicy::default()).html,
        include_str!("../../../../fixtures/plain/escaping.html")
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
            "<html lang=\"\">\n",
            "<head>\n",
            "<meta charset=\"utf-8\">\n",
            "<title>AdocWeave document</title>\n",
            "</head>\n",
            "<body>\n",
            "<p>paragraph</p>\n",
            "</body>\n",
            "</html>\n"
        )
    );
}

#[test]
fn complete_document_title_is_non_empty_and_escaped() {
    let policy = RenderPolicy {
        document_mode: HtmlDocumentMode::Complete,
        ..RenderPolicy::default()
    };
    let titled = parse("= <script> & title").expect("valid source");
    let formatted = parse("= *Formatted* title").expect("valid source");
    let untitled = parse("paragraph").expect("valid source");

    assert!(
        render(&titled.ast, &policy)
            .html
            .contains("<title>&lt;script&gt; &amp; title</title>")
    );
    assert!(
        render(&formatted.ast, &policy)
            .html
            .contains("<title>*Formatted* title</title>")
    );
    assert!(
        render(&untitled.ast, &policy)
            .html
            .contains("<title>AdocWeave document</title>")
    );
}

fn stylesheet_policy(sources: Vec<StylesheetSource>) -> RenderPolicy {
    RenderPolicy {
        document_mode: HtmlDocumentMode::Complete,
        stylesheets: StylesheetPolicy {
            sources,
            ..StylesheetPolicy::default()
        },
        ..RenderPolicy::default()
    }
}

#[test]
fn stylesheets_render_into_the_complete_document_head_in_host_order() {
    let parsed = parse(include_str!(
        "../../../../fixtures/html/head-stylesheets.adoc"
    ))
    .expect("valid source");
    let output = render(
        &parsed.ast,
        &stylesheet_policy(vec![
            StylesheetSource::External("https://example.com/a.css?a=1&b=2".to_owned()),
            StylesheetSource::Inline("p { margin: 0; }".to_owned()),
            StylesheetSource::External("https://example.com/a.css?a=1&b=2".to_owned()),
        ]),
    );

    assert_eq!(
        output.html,
        include_str!("../../../../fixtures/html/head-stylesheets.complete.html")
    );
    assert!(output.diagnostics.is_empty());
}

#[test]
fn stylesheets_are_not_mixed_into_fragment_output() {
    let parsed = parse("paragraph").expect("valid source");
    let output = render(
        &parsed.ast,
        &RenderPolicy {
            stylesheets: StylesheetPolicy {
                sources: vec![StylesheetSource::Inline("p {}".to_owned())],
                ..StylesheetPolicy::default()
            },
            ..RenderPolicy::default()
        },
    );

    assert_eq!(output.html, "<p>paragraph</p>\n");
    assert!(
        output
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.as_str() == "stylesheet-not-applicable")
    );
}

#[test]
fn inline_css_cannot_terminate_the_style_element() {
    let parsed = parse("paragraph").expect("valid source");
    for css in [
        "p {}</style><script>alert(1)</script>",
        "p {}</STYLE ><script>x</script>",
        "p {}<!-- boo",
        "p {}\u{0}",
    ] {
        let output = render(
            &parsed.ast,
            &stylesheet_policy(vec![StylesheetSource::Inline(css.to_owned())]),
        );

        assert!(!output.html.contains("<style"), "css {css:?} was emitted");
        assert!(!output.html.contains("script"));
        assert!(
            output
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code.as_str() == "invalid-stylesheet-content"),
            "css {css:?} was not rejected"
        );
    }
}

#[test]
fn stylesheet_urls_are_checked_by_the_active_urls_and_escaped() {
    let parsed = parse("paragraph").expect("valid source");
    for url in [
        "javascript:alert(1)",
        "data:text/css,p{}",
        "../theme.css",
        "//evil.example/theme.css",
    ] {
        let output = render(
            &parsed.ast,
            &stylesheet_policy(vec![StylesheetSource::External(url.to_owned())]),
        );

        assert!(!output.html.contains("<link"), "url {url:?} was emitted");
        assert!(
            output
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code.as_str() == "invalid-stylesheet-url"),
            "url {url:?} was not rejected"
        );
    }

    let output = render(
        &parsed.ast,
        &stylesheet_policy(vec![StylesheetSource::External(
            "https://example.com/a.css?x=\"1\"".to_owned(),
        )]),
    );
    assert!(!output.html.contains("<link"));
}

#[test]
fn stylesheet_limits_reject_oversized_and_excess_sources() {
    let parsed = parse("paragraph").expect("valid source");
    let policy = RenderPolicy {
        document_mode: HtmlDocumentMode::Complete,
        stylesheets: StylesheetPolicy {
            sources: vec![
                StylesheetSource::Inline("p { margin: 0; }".to_owned()),
                StylesheetSource::Inline("q { margin: 0; }".to_owned()),
                StylesheetSource::Inline("body { margin: 0; }".to_owned()),
            ],
            max_inline_bytes: 16,
            max_sources: 1,
            ..StylesheetPolicy::default()
        },
        ..RenderPolicy::default()
    };
    let output = render(&parsed.ast, &policy);

    assert_eq!(output.html.matches("<style>").count(), 1);
    assert!(output.html.contains("p { margin: 0; }"));
    assert!(
        output
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.as_str() == "stylesheet-limit-exceeded")
    );
    assert!(!output.html.contains("body { margin: 0; }"));
}

#[test]
fn html_contract_golden_covers_fragment_and_complete_document() {
    let parsed =
        parse(include_str!("../../../../fixtures/html/contract.adoc")).expect("valid source");
    assert_eq!(
        render(&parsed.ast, &RenderPolicy::default()).html,
        include_str!("../../../../fixtures/html/contract.fragment.html")
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
        include_str!("../../../../fixtures/html/contract.complete.html")
    );
}

#[test]
fn render_policy_allows_only_configured_safe_schemes() {
    let mut policy = RenderPolicy::default();
    assert_eq!(
        policy.classify_url("https://example.com", UrlProvenance::Authored),
        UrlDecision::Allowed
    );
    assert_eq!(
        policy.classify_url("HTTP://example.com", UrlProvenance::Authored),
        UrlDecision::Allowed
    );
    assert_eq!(
        policy.classify_url("javascript:alert(1)", UrlProvenance::Authored),
        UrlDecision::Rejected
    );
    assert_eq!(
        policy.classify_url("java%0ascript:alert(1)", UrlProvenance::Authored),
        UrlDecision::Rejected
    );
    assert_eq!(
        policy.classify_url("relative.adoc", UrlProvenance::Authored),
        UrlDecision::Rejected
    );
    assert_eq!(
        policy.classify_url("/absolute", UrlProvenance::Authored),
        UrlDecision::Rejected
    );
    assert_eq!(
        policy.classify_url("data:text/html,x", UrlProvenance::Authored),
        UrlDecision::Rejected
    );

    policy
        .active_urls
        .allowed_schemes
        .insert("mailto".to_owned());
    policy.active_urls.allow_authored_relative = true;
    assert!(policy.allows_url("mailto:user@example.com", UrlProvenance::Authored));
    assert!(policy.allows_url("relative.adoc", UrlProvenance::Authored));
    assert!(!policy.allows_url("../outside.adoc", UrlProvenance::Authored));

    let parsed = parse("link:relative.adoc[relative]").expect("parse");
    assert_eq!(
        render(&parsed.ast, &policy).html,
        "<p><a href=\"relative.adoc\">relative</a></p>\n"
    );
}

#[test]
fn external_link_attributes_are_fixed_and_do_not_apply_to_xrefs() {
    let analysis = crate::core::Engine::new(crate::core::AnalysisOptions::default())
        .analyze("https://example.com/[External] xref:note:123[Internal]")
        .expect("analysis");
    let policy = RenderPolicy {
        external_links: ExternalLinkPresentation::NewContext { noreferrer: true },
        ..RenderPolicy::default()
    };
    let output = render_with_inputs(
        analysis.ast(),
        &policy,
        &RenderInputs::default().with_references(vec![ResolvedReference::resolved(
            analysis.references()[0].range,
            "https://app.example/notes/123",
        )]),
    );

    assert!(
        output.html.contains(
            "href=\"https://example.com/\" target=\"_blank\" rel=\"noopener noreferrer\""
        )
    );
    assert!(
        output
            .html
            .contains("<a href=\"https://app.example/notes/123\">Internal</a>")
    );
}

#[test]
fn source_math_reference_and_resource_policies_fail_closed() {
    let source = "[source,python]\n----\nprint(1)\n----\n\nstem:[x] xref:note:secret[] image:https://example/x.png[alt]";
    let analysis = crate::core::Engine::new(crate::core::AnalysisOptions::default())
        .analyze(source)
        .expect("analysis");
    let image = analysis.resource_queries()[0].reference.range();
    let policy = RenderPolicy {
        source_languages: SourceLanguagePolicy {
            allowed: Some(["rust".to_owned()].into_iter().collect()),
            unknown: UnknownSourceLanguage::Diagnostic,
        },
        math_languages: MathLanguagePolicy {
            allowed: std::collections::BTreeSet::new(),
        },
        unresolved_references: UnresolvedReferencePresentation::LabelOnly,
        resources: ResourceCapabilities {
            images: false,
            media: false,
        },
        ..RenderPolicy::default()
    };
    let output = render_with_inputs(
        analysis.ast(),
        &policy,
        &RenderInputs::default().with_resources(vec![ResolvedResource::resolved(
            image,
            "https://cdn.example/x.png",
            "image/png".parse().expect("media type"),
            None,
        )]),
    );

    assert!(!output.html.contains("language-python"));
    assert!(!output.html.contains("math-latex"));
    assert!(!output.html.contains("note:secret"));
    assert!(!output.html.contains("<img"));
    let codes = output
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.as_str())
        .collect::<Vec<_>>();
    assert!(codes.contains(&"source-language-not-allowed"));
    assert!(codes.contains(&"math-language-not-allowed"));
    assert!(codes.contains(&"resource-capability-disabled"));
}

#[test]
fn resolved_reference_notices_are_projected_as_render_diagnostics() {
    let analysis = crate::core::Engine::new(crate::core::AnalysisOptions::default())
        .analyze("xref:note:123#missing[Note]")
        .expect("analysis");
    let output = render_with_inputs(
        analysis.ast(),
        &RenderPolicy::default(),
        &RenderInputs::default().with_references(vec![
            ResolvedReference::resolved(
                analysis.references()[0].range,
                "https://app.example/notes/123",
            )
            .with_notices(vec![crate::reference::ResolutionNotice {
                kind: crate::reference::ResolutionNoticeKind::Fallback,
            }]),
        ]),
    );

    assert_eq!(
        output.diagnostics[0].code.as_str(),
        "reference-resolution-fallback"
    );
}

#[test]
fn kind_only_reference_failure_uses_a_fixed_diagnostic() {
    let analysis = crate::core::Engine::new(crate::core::AnalysisOptions::default())
        .analyze("xref:record:private[Public label]")
        .expect("analysis");
    let output = render_with_inputs(
        analysis.ast(),
        &RenderPolicy::default(),
        &RenderInputs::default().with_references(vec![ResolvedReference::failed(
            analysis.references()[0].range,
            crate::reference::ResolverFailure {
                kind: crate::reference::ResolutionFailureKind::MissingTarget,
            },
        )]),
    );

    assert_eq!(output.html, "<p>Public label</p>\n");
    assert_eq!(
        output.diagnostics[0].code.as_str(),
        "missing-reference-target"
    );
    assert_eq!(output.diagnostics[0].message, "reference resolution failed");
}

#[test]
fn resolved_display_text_is_plain_text_and_only_fills_an_empty_label() {
    let analysis = crate::core::Engine::new(crate::core::AnalysisOptions::default())
        .analyze(
            "xref:note:01800000-0000-7000-8000-000000000001[]\n\n\
             xref:note:01800000-0000-7000-8000-000000000002[Authored *label*]",
        )
        .expect("analysis");
    let inputs = RenderInputs::default().with_references(vec![
        ResolvedReference::resolved(
            analysis.references()[0].range,
            "/notes/01800000-0000-7000-8000-000000000001",
        )
        .with_display_text("公開 <タイトル> & *not markup*"),
        ResolvedReference::resolved(
            analysis.references()[1].range,
            "/notes/01800000-0000-7000-8000-000000000002",
        )
        .with_display_text("Resolver title must not replace the authored label"),
    ]);

    let output = render_with_inputs(
        analysis.ast(),
        &RenderPolicy {
            active_urls: crate::url::ActiveUrlPolicy {
                allow_resolved_root_relative: true,
                ..crate::url::ActiveUrlPolicy::default()
            },
            ..RenderPolicy::default()
        },
        &inputs,
    );

    assert_eq!(
        output.html,
        "<p><a href=\"/notes/01800000-0000-7000-8000-000000000001\">公開 &lt;タイトル&gt; &amp; *not markup*</a></p>\n\
         <p><a href=\"/notes/01800000-0000-7000-8000-000000000002\">Authored <strong>label</strong></a></p>\n"
    );
}

#[test]
fn failed_empty_label_hides_the_target_in_label_only_mode() {
    let analysis = crate::core::Engine::new(crate::core::AnalysisOptions::default())
        .analyze("xref:note:private[]")
        .expect("analysis");
    let inputs = RenderInputs::default().with_references(vec![ResolvedReference::failed(
        analysis.references()[0].range,
        crate::reference::ResolverFailure {
            kind: crate::reference::ResolutionFailureKind::MissingTarget,
        },
    )]);

    let output = render_with_inputs(
        analysis.ast(),
        &RenderPolicy {
            unresolved_references: UnresolvedReferencePresentation::LabelOnly,
            ..RenderPolicy::default()
        },
        &inputs,
    );

    assert_eq!(output.html, "<p></p>\n");
    assert!(!output.html.contains("private"));
}

#[test]
fn html_contract_has_explicit_allowlists() {
    assert_eq!(crate::VERSION, env!("CARGO_PKG_VERSION"));
    assert_eq!(
        ALLOWED_ELEMENTS,
        [
            "a",
            "audio",
            "body",
            "blockquote",
            "br",
            "caption",
            "cite",
            "code",
            "dd",
            "div",
            "dl",
            "dt",
            "em",
            "figcaption",
            "figure",
            "h1",
            "h2",
            "h3",
            "h4",
            "h5",
            "head",
            "hr",
            "html",
            "img",
            "kbd",
            "li",
            "link",
            "mark",
            "meta",
            "ol",
            "p",
            "pre",
            "span",
            "strong",
            "style",
            "sub",
            "sup",
            "table",
            "tbody",
            "td",
            "tfoot",
            "th",
            "thead",
            "title",
            "tr",
            "ul",
            "video"
        ]
    );
    assert_eq!(
        ALLOWED_ATTRIBUTES,
        [
            "alt",
            "charset",
            "class",
            "colspan",
            "controls",
            "data-language",
            "data-line-numbers",
            "data-line-start",
            "data-math-display",
            "data-math-language",
            "height",
            "href",
            "id",
            "lang",
            "poster",
            "rel",
            "rowspan",
            "src",
            "target",
            "title",
            "width"
        ]
    );
    assert_eq!(
        ALLOWED_CLASSES,
        [
            "author",
            "admonition",
            "admonition-caution",
            "admonition-important",
            "admonition-note",
            "admonition-tip",
            "admonition-warning",
            "attribution",
            "appendix",
            "bibliography-anchor",
            "bibliography-backref",
            "button",
            "callout-list",
            "callout-number",
            "checklist-marker",
            "citation",
            "document-title",
            "footnote",
            "footnote-backref",
            "footnote-ref",
            "footnotes",
            "index-term",
            "language-*",
            "lead",
            "math-latex",
            "math-typst",
            "menu",
            "page-break",
            "revision",
            "quote",
            "source-block",
            "table-align-center",
            "table-align-left",
            "table-align-right",
            "table-valign-bottom",
            "table-valign-middle",
            "table-valign-top",
            "table-frame-all",
            "table-frame-ends",
            "table-frame-none",
            "table-frame-sides",
            "table-grid-all",
            "table-grid-cols",
            "table-grid-none",
            "table-grid-rows",
            "table-stripes-all",
            "table-stripes-even",
            "table-stripes-hover",
            "table-stripes-none",
            "table-stripes-odd",
            "toc",
            "title",
            "verse"
        ]
    );
    let parsed = parse("paragraph").expect("parse");
    assert_eq!(
        render(&parsed.ast, &RenderPolicy::default()).package_version,
        crate::VERSION
    );
}

fn output_inventory(html: &str) -> (BTreeSet<String>, BTreeSet<String>, BTreeSet<String>) {
    let mut elements = BTreeSet::new();
    let mut attributes = BTreeSet::new();
    let mut classes = BTreeSet::new();
    let mut remaining = html;
    while let Some(start) = remaining.find('<') {
        remaining = &remaining[start + 1..];
        let Some(end) = remaining.find('>') else {
            break;
        };
        let tag = &remaining[..end];
        remaining = &remaining[end + 1..];
        let tag = tag.trim_start();
        if tag.starts_with('!') {
            continue;
        }
        let tag = tag.strip_prefix('/').unwrap_or(tag);
        let name_end = tag.find(char::is_whitespace).unwrap_or(tag.len());
        let name = &tag[..name_end];
        elements.insert(name.to_owned());
        if name_end == tag.len() || tag.starts_with('/') {
            continue;
        }
        let mut rest = &tag[name_end..];
        while !rest.trim_start().is_empty() {
            rest = rest.trim_start();
            let attribute_end = rest
                .find(|character: char| character.is_whitespace() || character == '=')
                .unwrap_or(rest.len());
            let attribute = &rest[..attribute_end];
            attributes.insert(attribute.to_owned());
            rest = &rest[attribute_end..];
            if let Some(value) = rest.strip_prefix("=\"") {
                let value_end = value.find('"').expect("serialized attribute is quoted");
                if attribute == "class" {
                    classes.extend(
                        value[..value_end]
                            .split_ascii_whitespace()
                            .map(str::to_owned),
                    );
                }
                rest = &value[value_end + 1..];
            }
        }
    }
    (elements, attributes, classes)
}

#[test]
fn generated_output_inventory_is_within_the_public_allowlists() {
    let parsed = parse(
        "= Inventory\n:toc:\n\n== Section\n\n[NOTE]\n====\nbody\n====\n\n[source,rust,linenums]\n----\nfn main() {}\n----\n\n[cols=\"1,1\",options=\"header\"]\n|===\n|a |b\n|c |d\n|===\n\n* item\n",
    )
    .expect("parse");
    let html = render(&parsed.ast, &RenderPolicy::default()).html;
    let (elements, attributes, classes) = output_inventory(&html);

    assert!(!elements.is_empty());
    assert!(!attributes.is_empty());
    assert!(!classes.is_empty());
    assert!(
        elements
            .iter()
            .all(|element| ALLOWED_ELEMENTS.contains(&element.as_str())),
        "unexpected elements: {elements:?}"
    );
    assert!(
        attributes
            .iter()
            .all(|attribute| ALLOWED_ATTRIBUTES.contains(&attribute.as_str())),
        "unexpected attributes: {attributes:?}"
    );
    assert!(
        classes.iter().all(|class| {
            ALLOWED_CLASSES.contains(&class.as_str())
                || (ALLOWED_CLASSES.contains(&"language-*")
                    && class.starts_with("language-")
                    && class.len() > "language-".len())
        }),
        "unexpected classes: {classes:?}"
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

    let parsed =
        parse("[#safe.evil%interactive,onclick=\"alert(1)\",style=\"display:none\"]\nText\n")
            .expect("metadata source");
    let html = render(&parsed.ast, &RenderPolicy::default()).html;
    assert_eq!(html, "<p id=\"safe\">Text</p>\n");
    assert!(!html.contains("onclick"));
    assert!(!html.contains("display:none"));
    assert!(!html.contains("evil"));

    let parsed = parse(
        "++++\n<script>alert(1)</script>\n++++\n\n////\n<script>hidden</script>\n////\n\n====\ninside *safe*\n====\n",
    )
    .expect("delimited source");
    let html = render(&parsed.ast, &RenderPolicy::default()).html;
    assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(!html.contains("<script>"));
    assert!(!html.contains("hidden"));
    assert!(html.contains("<p>inside <strong>safe</strong></p>"));
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
fn links_apply_attributes_labels_and_active_urls() {
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
fn link_target_attributes_expand_recursively() {
    let parsed = parse("= Links\n:b: expanded\n:a: {b}\n\nhttps://example.com/{a}[target]\n")
        .expect("parse");
    let AstBlock::Paragraph(paragraph) = &parsed.ast.blocks()[1] else {
        panic!("paragraph");
    };
    let Inline::Link(link) = &paragraph.inlines[0] else {
        panic!("link");
    };

    assert_eq!(link.target, "https://example.com/expanded");
}

#[test]
fn ordered_substitutions_render_styles_replacements_and_safe_passthroughs() {
    let parsed = parse("= Pipeline\n:b: value\n:a: {b}\n\n{a} #mark# H~2~O E=mc^2^ \"`double`\" (C) ... +<b>*raw*</b>+\n\n++++\n<script>alert(1)</script>\n++++\n").expect("parse");
    let html = render(&parsed.ast, &RenderPolicy::default()).html;
    assert!(html.contains("<p>value <mark>mark</mark> H<sub>2</sub>O E=mc<sup>2</sup> “double” © … &lt;b&gt;*raw*&lt;/b&gt;</p>"));
    assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(!html.contains("<script>"));
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
                        if matches!(reference.target, Some(ReferenceKey::Document { .. })) =>
                    {
                        Some(reference.range)
                    }
                    _ => None,
                })
            }
            _ => None,
        })
        .expect("external reference");
    let output = render_with_inputs(
        &parsed.ast,
        &RenderPolicy::default(),
        &RenderInputs::default().with_references(vec![ResolvedReference::resolved(
            external,
            "https://notes.example/part",
        )]),
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
    let source = include_str!("../../../../fixtures/heading/basic.adoc");
    let parsed = parse(source).expect("valid source");
    let output = render(&parsed.ast, &RenderPolicy::default());

    assert_eq!(
        output.html,
        include_str!("../../../../fixtures/heading/basic.html")
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
    let target_ids = crate::document::reference_targets_ast(&parsed.ast)
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
fn ordered_list_multiline_principal_text_stays_in_each_item() {
    let parsed = parse(". ほげ\n  ほげ\n. ほが\n  ほが\n").expect("parse");
    let output = render(&parsed.ast, &RenderPolicy::default());

    assert_eq!(
        output.html,
        "<ol>\n<li>ほげ   ほげ</li>\n<li>ほが   ほが</li>\n</ol>\n"
    );
}

#[test]
fn ordered_list_multiline_principal_text_renders_an_explicit_hard_break() {
    let parsed = parse(". first +\ncontinued\n. second\n").expect("parse");
    let output = render(&parsed.ast, &RenderPolicy::default());

    assert_eq!(
        output.html,
        "<ol>\n<li>first<br>\ncontinued</li>\n<li>second</li>\n</ol>\n"
    );
}

#[test]
fn lists_match_the_supported_asciidoctor_fixture() {
    let parsed = parse(include_str!(
        "../../../../fixtures/lists/asciidoctor-compatible.adoc"
    ))
    .expect("parse");
    let output = render(&parsed.ast, &RenderPolicy::default());

    assert_eq!(
        output.html,
        include_str!("../../../../fixtures/lists/asciidoctor-compatible.html")
    );
}

#[test]
fn standard_list_forms_render_semantic_html() {
    let parsed = parse(include_str!(
        "../../../../fixtures/lists/standard-forms.adoc"
    ))
    .expect("parse");
    let output = render(&parsed.ast, &RenderPolicy::default());

    assert_eq!(
        output.html,
        include_str!("../../../../fixtures/lists/standard-forms.html")
    );
}

#[test]
fn ordered_list_html_does_not_bypass_the_public_attribute_allowlist() {
    let parsed = parse("[start=3,%reversed,upperroman]\n. one\n. two\n").expect("parse");
    let output = render(&parsed.ast, &RenderPolicy::default());

    assert_eq!(output.html, "<ol>\n<li>one</li>\n<li>two</li>\n</ol>\n");
}

#[test]
fn standard_table_forms_render_allowlisted_semantic_html() {
    let parsed = parse(include_str!(
        "../../../../fixtures/tables/standard-forms.adoc"
    ))
    .expect("parse");
    let output = render(&parsed.ast, &RenderPolicy::default());

    assert_eq!(
        output.html,
        include_str!("../../../../fixtures/tables/standard-forms.html")
    );
}

#[test]
fn table_presentation_renders_only_fixed_classes_and_caption() {
    let parsed =
        parse(".Example\n[frame=ends,grid=rows,stripes=even,width=75%]\n|===\n|value\n|===\n")
            .expect("parse");
    let output = render(&parsed.ast, &RenderPolicy::default());

    assert_eq!(
        output.html,
        "<table class=\"table-frame-ends table-grid-rows table-stripes-even\" width=\"75%\">\n<caption>Example</caption>\n<tbody>\n<tr>\n<td class=\"table-align-left table-valign-top\">value</td>\n</tr>\n</tbody>\n</table>\n"
    );
}

#[test]
fn table_presentation_uses_the_first_attribute_and_accepts_unitless_width() {
    let parsed = parse("[frame=ends,frame=sides,width=75]\n|===\n|value\n|===\n").expect("parse");
    let output = render(&parsed.ast, &RenderPolicy::default());

    assert_eq!(
        output.html,
        "<table class=\"table-frame-ends table-grid-all table-stripes-none\" width=\"75%\">\n<tbody>\n<tr>\n<td class=\"table-align-left table-valign-top\">value</td>\n</tr>\n</tbody>\n</table>\n"
    );
}

#[test]
fn advanced_table_formats_and_asciidoc_cells_render_from_typed_content() {
    let source = "[format=csv,options=header]\n|===\nname,value\nalpha,\"one, two\"\n|===\n\n[cols=a]\n|===\n|Paragraph.\n\n* one\n* two\n|===\n";
    let parsed = parse(source).expect("parse");
    let output = render(&parsed.ast, &RenderPolicy::default());
    assert!(output.html.contains("<thead>"));
    assert!(
        output
            .html
            .contains("<td class=\"table-align-left table-valign-top\">one, two</td>")
    );
    assert!(
        output
            .html
            .contains("<td class=\"table-align-left table-valign-top\"><p>Paragraph.</p>\n<ul>")
    );
    assert!(output.html.contains("<li>one</li>"));
}

#[test]
fn asciidoc_table_cells_preserve_comment_like_verbatim_lines() {
    let source = "[cols=a]\n|===\na|....\n// literal must remain\n\n....\n|===\n";
    let parsed = parse(source).expect("parse");
    let output = render(&parsed.ast, &RenderPolicy::default());

    assert!(
        output
            .html
            .contains("<pre>// literal must remain\n\n</pre>")
    );
}

#[test]
fn asciidoc_cells_lower_block_presentations_like_root_blocks() {
    let source = "[cols=a]\n|===\na|[TIP]\n====\ncell *tip*.\n====\n|===\n\n[cols=a]\n|===\na|[quote,Author,Work]\n____\ncell *quote*.\n____\n|===\n\n[cols=a]\n|===\na|[verse,Poet,Poem]\n____\nline one\nline two\n____\n|===\n";
    let parsed = parse(source).expect("parse");
    let output = render(&parsed.ast, &RenderPolicy::default());

    assert!(
        output
            .html
            .contains("<div class=\"admonition admonition-tip\">")
    );
    assert!(output.html.contains("<div class=\"quote\">"));
    assert!(output.html.contains("<div class=\"verse\">"));
    assert!(output.html.contains("<cite>Work</cite>"));
    assert!(output.html.contains("<cite>Poem</cite>"));
}

#[test]
fn standard_macros_render_resources_through_the_html_policy() {
    let parsed = parse(include_str!("../../../../fixtures/macros/standard.adoc")).expect("parse");
    let output = render_with_inputs(
        &parsed.ast,
        &RenderPolicy::default(),
        &echo_resource_inputs(&parsed.ast),
    );
    assert_eq!(
        output.html,
        include_str!("../../../../fixtures/macros/standard.html")
    );

    let parsed = parse("kbd:[Ctrl+C] btn:[Save] menu:File[Open]").expect("parse");
    let output = render(
        &parsed.ast,
        &RenderPolicy {
            render_ui_macros: true,
            ..RenderPolicy::default()
        },
    );
    assert_eq!(
        output.html,
        "<p><kbd>Ctrl+C</kbd> <span class=\"button\">Save</span> <span class=\"menu\">File › Open</span></p>\n"
    );

    let unsafe_resource = parse("image:javascript:alert(1)[safe fallback]").expect("parse");
    let output = render_with_inputs(
        &unsafe_resource.ast,
        &RenderPolicy::default(),
        &echo_resource_inputs(&unsafe_resource.ast),
    );
    assert_eq!(output.html, "<p>safe fallback</p>\n");
    assert!(!output.html.contains("<img"));
    assert_eq!(output.diagnostics[0].code.as_str(), "invalid-url-scheme");
}

#[test]
fn mismatched_media_types_do_not_emit_active_elements() {
    let parsed = parse("image:https://example.org/diagram.bin[Diagram]").expect("parse");
    let range = crate::resource::ResourceReference::from_macro(
        parsed
            .ast
            .blocks
            .iter()
            .find_map(|block| match block {
                AstBlock::Paragraph(paragraph) => paragraph.inlines.iter().find_map(|inline| {
                    if let Inline::Macro(node) = inline {
                        Some(node)
                    } else {
                        None
                    }
                }),
                _ => None,
            })
            .expect("macro"),
    )[0]
    .range();
    let output = render_with_inputs(
        &parsed.ast,
        &RenderPolicy::default(),
        &RenderInputs::default().with_resources(vec![ResolvedResource::resolved(
            range,
            "https://cdn.example/diagram.bin",
            MediaType::parse("audio/mpeg").expect("media type"),
            None,
        )]),
    );
    assert_eq!(output.html, "<p>Diagram</p>\n");
    assert_eq!(
        output.diagnostics[0].code.as_str(),
        "resource-media-type-mismatch"
    );
}

#[test]
fn media_attributes_and_independently_resolved_poster_are_allowlisted() {
    let parsed = parse(
        "video:https://example.org/demo.mp4[Demo,640,360,poster=https://example.org/poster.jpg,title=Presentation]",
    )
    .expect("parse");
    let inputs = echo_resource_inputs(&parsed.ast);
    assert_eq!(inputs.resources().len(), 2);
    let output = render_with_inputs(&parsed.ast, &RenderPolicy::default(), &inputs);
    assert_eq!(
        output.html,
        "<p><video src=\"https://example.org/demo.mp4\" controls width=\"640\" height=\"360\" poster=\"https://example.org/poster.jpg\" title=\"Presentation\"></video></p>\n"
    );
    assert!(output.diagnostics.is_empty());

    let primary_only = RenderInputs::default().with_resources(vec![inputs.resources()[0].clone()]);
    let output = render_with_inputs(&parsed.ast, &RenderPolicy::default(), &primary_only);
    assert!(
        output
            .html
            .contains("<video src=\"https://example.org/demo.mp4\" controls")
    );
    assert!(!output.html.contains(" poster="));
    assert_eq!(output.diagnostics[0].code.as_str(), "unresolved-resource");

    let poster_disabled = render_with_inputs(
        &parsed.ast,
        &RenderPolicy {
            resources: ResourceCapabilities {
                images: false,
                media: true,
            },
            ..RenderPolicy::default()
        },
        &inputs,
    );
    assert!(poster_disabled.html.contains("<video "));
    assert!(!poster_disabled.html.contains(" poster="));
    assert_eq!(
        poster_disabled.diagnostics[0].code.as_str(),
        "resource-capability-disabled"
    );

    let mismatched_poster = RenderInputs::default().with_resources(vec![
        inputs.resources()[0].clone(),
        ResolvedResource::resolved(
            inputs.resources()[1].source_range,
            "https://example.org/poster.mp3",
            "audio/mpeg".parse().expect("media type"),
            None,
        ),
    ]);
    let output = render_with_inputs(&parsed.ast, &RenderPolicy::default(), &mismatched_poster);
    assert!(output.html.contains("<video "));
    assert!(!output.html.contains(" poster="));
    assert_eq!(
        output.diagnostics[0].code.as_str(),
        "resource-media-type-mismatch"
    );

    let empty = parse("video:https://example.org/demo.mp4[Demo,poster=]").expect("parse");
    let output = render_with_inputs(
        &empty.ast,
        &RenderPolicy::default(),
        &echo_resource_inputs(&empty.ast),
    );
    assert!(output.html.contains("<video "));
    assert!(!output.html.contains(" poster="));
    assert!(output.diagnostics.is_empty());
}

#[test]
fn render_inputs_handle_missing_failed_duplicate_and_unused_resources_deterministically() {
    let parsed = parse("image:https://source.example/image.png[alt]").expect("parse");
    let resolved = echo_resource_inputs(&parsed.ast).resources()[0].clone();

    let missing = render(&parsed.ast, &RenderPolicy::default());
    assert_eq!(missing.html, "<p>alt</p>\n");
    assert_eq!(missing.diagnostics[0].code.as_str(), "unresolved-resource");

    let failed = ResolvedResource::failed(
        resolved.source_range,
        crate::resource::ResourceFailure {
            kind: crate::resource::ResourceFailureKind::PermissionDenied,
        },
    );
    let failed = render_with_inputs(
        &parsed.ast,
        &RenderPolicy::default(),
        &RenderInputs::default().with_resources(vec![failed]),
    );
    assert_eq!(
        failed.diagnostics[0].code.as_str(),
        "resource-permission-denied"
    );

    let duplicate = render_with_inputs(
        &parsed.ast,
        &RenderPolicy::default(),
        &RenderInputs::default().with_resources(vec![resolved.clone(), resolved.clone()]),
    );
    assert_eq!(
        duplicate.diagnostics[0].code.as_str(),
        "duplicate-render-input"
    );

    let unused_range =
        crate::source::TextRange::new(crate::source::TextSize::ZERO, crate::source::TextSize::ZERO)
            .expect("range");
    let unused = render_with_inputs(
        &parsed.ast,
        &RenderPolicy::default(),
        &RenderInputs::default().with_resources(vec![
            resolved,
            ResolvedResource::resolved(
                unused_range,
                "https://unused.example/image.png",
                "image/png".parse().expect("media type"),
                None,
            ),
        ]),
    );
    assert!(unused.html.contains("<img"));
    assert_eq!(unused.diagnostics[0].code.as_str(), "unused-render-input");
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
        parse(include_str!("../../../../fixtures/stem/substitutions.adoc")).expect("parse");
    let output = render(&parsed.ast, &RenderPolicy::default());

    assert_eq!(
        output.html,
        include_str!("../../../../fixtures/stem/substitutions.html")
    );
    assert!(!output.html.contains("<z>"));
}

#[test]
fn math_contract_exposes_language_and_display_without_executing_source() {
    let parsed = parse(
        "latexmath:[<script>alert(1)</script>]\n\n[latexmath]\n++++\n<img src=x onerror=alert(1)>\n++++\n",
    )
    .expect("parse math contract fixture");
    let output = render(&parsed.ast, &RenderPolicy::default());

    assert!(output.html.contains(
        "<code class=\"math-latex\" data-math-language=\"latexmath\" data-math-display=\"inline\">"
    ));
    assert!(output.html.contains(
        "<pre class=\"math-latex\" data-math-language=\"latexmath\" data-math-display=\"block\"><code>"
    ));
    assert!(!output.html.contains("<script>"));
    assert!(!output.html.contains("<img"));
    assert!(output.html.contains("&lt;script&gt;"));
    assert!(output.html.contains("&lt;img src=x onerror=alert(1)&gt;"));
}
