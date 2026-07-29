//! Pure Hover projection over an adopted analysis snapshot.

use adocweave::Analysis;
use adocweave::preprocess::AnalysisProjection;
use adocweave::semantic as parser;
use adocweave::semantic::{
    DocumentElement, Inline, MathLanguage, document_element_at, generate_heading_ids,
};
use adocweave::text::TextRange;
use async_lsp::lsp_types as lsp;

use crate::position::{PositionEncoding, range_contains_offset, range_to_lsp};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum HoverPresentation {
    #[default]
    Legacy,
    Markdown,
    PlainText,
}

pub(crate) fn hover<'a>(
    analysis: &Analysis,
    uri: &lsp::Url,
    offset: u32,
    projections: impl IntoIterator<Item = &'a AnalysisProjection>,
    encoding: PositionEncoding,
    presentation: HoverPresentation,
) -> Result<Option<lsp::Hover>, String> {
    if let Some(attribute) = analysis
        .document_attribute_occurrences()
        .iter()
        .find(|attribute| range_contains_offset(attribute.range, offset))
    {
        return make_hover(
            format!(
                "**document attribute**  \nName: `{}`\n\nSource value:\n\n    {}\n\nFolded value:\n\n    {}",
                attribute.name,
                attribute.value.source_text.replace('\n', "\n    "),
                attribute.value.folded_text.replace('\n', "\n    ")
            ),
            attribute.range,
            analysis,
            encoding,
            presentation,
        );
    }
    for projection in projections {
        if let Some((reference, origin)) = projected_attribute_reference_at(projection, uri, offset)
        {
            return make_hover(
                attribute_reference_hover(reference),
                origin.range.text_range(),
                analysis,
                encoding,
                presentation,
            );
        }
    }
    if let Some(reference) = analysis
        .attribute_references()
        .iter()
        .find(|reference| range_contains_offset(reference.range, offset))
    {
        return make_hover(
            attribute_reference_hover(reference),
            reference.range,
            analysis,
            encoding,
            presentation,
        );
    }
    if let Some(target) = analysis.reference_targets().iter().find(|target| {
        range_contains_offset(target.id_range, offset)
            && !analysis.document().blocks().iter().any(|block| {
                matches!(
                    block,
                    parser::Block::Heading(heading) if heading.text_range == target.id_range
                )
            })
    }) {
        return make_hover(
            format!("**reference target**  \nID: `{}`", target.id),
            target.id_range,
            analysis,
            encoding,
            presentation,
        );
    }
    if let Some((value, range)) = inline_hover(analysis.document(), offset) {
        return make_hover(value, range, analysis, encoding, presentation);
    }
    if let Some((value, range)) = block_presentation_hover(analysis.document(), offset) {
        return make_hover(value, range, analysis, encoding, presentation);
    }
    for author in &analysis.document().header().authors {
        if range_contains_offset(author.range, offset) {
            let value = author.email.as_ref().map_or_else(
                || format!("**author**  \nName: `{}`", author.name),
                |email| format!("**author**  \nName: `{}`  \nEmail: `{email}`", author.name),
            );
            return make_hover(value, author.range, analysis, encoding, presentation);
        }
    }
    if let Some(revision) = &analysis.document().header().revision
        && range_contains_offset(revision.range, offset)
    {
        return make_hover(
            "**document revision**".to_owned(),
            revision.range,
            analysis,
            encoding,
            presentation,
        );
    }
    let Some(element) = document_element_at(analysis.document(), offset) else {
        return Ok(None);
    };
    let metadata_hover = match element {
        DocumentElement::MetadataTitle(value) => {
            Some(("block title", value.value.as_str(), value.range))
        }
        DocumentElement::MetadataId(value) => Some(("block ID", value.value.as_str(), value.range)),
        DocumentElement::MetadataRole(value) => {
            Some(("block role", value.value.as_str(), value.range))
        }
        DocumentElement::MetadataOption(value) => {
            Some(("block option", value.value.as_str(), value.range))
        }
        DocumentElement::ElementAttribute(attribute) => Some((
            attribute.name.as_deref().unwrap_or("positional attribute"),
            attribute.value.as_str(),
            attribute.range,
        )),
        _ => None,
    };
    if let Some((kind, value, range)) = metadata_hover {
        return make_hover(
            format!("**{kind}**  \nValue: `{value}`"),
            range,
            analysis,
            encoding,
            presentation,
        );
    }
    let (heading, range, part) = match element {
        DocumentElement::HeadingMarker(heading) => (heading, heading.marker_range, "marker"),
        DocumentElement::HeadingText(heading) => (heading, heading.text_range, "text"),
        DocumentElement::SourceLanguage(_) | DocumentElement::SourceAttribute(_) => {
            return Ok(None);
        }
        DocumentElement::MetadataTitle(_)
        | DocumentElement::MetadataId(_)
        | DocumentElement::MetadataRole(_)
        | DocumentElement::MetadataOption(_)
        | DocumentElement::ElementAttribute(_) => unreachable!(),
    };
    let id = generate_heading_ids(analysis.document())
        .iter()
        .find(|candidate| candidate.range == heading.text_range)
        .map(|candidate| candidate.id.clone())
        .unwrap_or_else(|| "_section".to_owned());
    let level = match heading.kind {
        parser::HeadingKind::DocumentTitle => "document title".to_owned(),
        parser::HeadingKind::Part => "book part".to_owned(),
        parser::HeadingKind::Section { level } => format!("section level {level}"),
        parser::HeadingKind::Discrete { level } => format!("discrete heading level {level}"),
    };
    make_hover(
        format!("**{level}**  \nGenerated ID: `{id}`  \nPart: {part}"),
        range,
        analysis,
        encoding,
        presentation,
    )
}

fn make_hover(
    value: String,
    range: TextRange,
    analysis: &Analysis,
    encoding: PositionEncoding,
    presentation: HoverPresentation,
) -> Result<Option<lsp::Hover>, String> {
    let contents = match presentation {
        HoverPresentation::Markdown => lsp::HoverContents::Markup(lsp::MarkupContent {
            kind: lsp::MarkupKind::Markdown,
            value,
        }),
        HoverPresentation::PlainText => lsp::HoverContents::Markup(lsp::MarkupContent {
            kind: lsp::MarkupKind::PlainText,
            value: hover_plain_text(&value),
        }),
        HoverPresentation::Legacy => {
            lsp::HoverContents::Scalar(lsp::MarkedString::String(hover_plain_text(&value)))
        }
    };
    Ok(Some(lsp::Hover {
        contents,
        range: Some(range_to_lsp(range, analysis.source_document(), encoding)?),
    }))
}

fn hover_plain_text(markdown: &str) -> String {
    markdown
        .replace("  \n", "\n")
        .replace("**", "")
        .replace('`', "")
}

fn attribute_reference_hover(reference: &adocweave::semantic::AttributeReference) -> String {
    match &reference.value {
        Ok(Some(value)) => format!(
            "**attribute reference**  \nName: `{}`  \nValue: `{}`",
            reference.name, value
        ),
        Ok(None) => format!(
            "**attribute reference**  \nName: `{}`  \nValue: _unset_",
            reference.name
        ),
        Err(error) => format!(
            "**attribute reference**  \nName: `{}`  \nResolution: `{}`",
            reference.name,
            match error {
                adocweave::semantic::AttributeExpansionError::Undefined => "undefined",
                adocweave::semantic::AttributeExpansionError::Cycle => "cycle",
                adocweave::semantic::AttributeExpansionError::DepthLimitExceeded => "depth limit",
                adocweave::semantic::AttributeExpansionError::SizeLimitExceeded => "size limit",
            }
        ),
    }
}

fn projected_attribute_reference_at<'a>(
    projection: &'a AnalysisProjection,
    uri: &lsp::Url,
    offset: u32,
) -> Option<(
    &'a adocweave::semantic::AttributeReference,
    &'a adocweave::preprocess::SourceOrigin,
)> {
    projection
        .attribute_references
        .iter()
        .find_map(|reference| {
            reference
                .origins
                .iter()
                .find(|origin| {
                    origin
                        .source_id
                        .as_ref()
                        .is_some_and(|source_id| source_id.as_str() == uri.as_str())
                        && range_contains_offset(origin.range.text_range(), offset)
                })
                .map(|origin| (&reference.value, origin))
        })
}

fn inline_hover(
    document: &adocweave::semantic::Document,
    offset: u32,
) -> Option<(String, TextRange)> {
    let mut found = None;
    adocweave::semantic::walk(document, |node| {
        let adocweave::semantic::SemanticNode::Inline(inline) = node else {
            return;
        };
        if range_contains_offset(inline.range(), offset) {
            let value = match inline {
                Inline::Link(link) => {
                    Some(format!("**external link**  \nTarget: `{}`", link.target))
                }
                Inline::Reference(reference) => Some(format!(
                    "**cross reference**  \nTarget: `{}`",
                    reference.target_source
                )),
                Inline::Formula(formula) => Some(format!(
                    "**{} formula**  \nContent: `{}`",
                    match formula.language {
                        MathLanguage::Latex => "LaTeX",
                        MathLanguage::Typst => "Typst",
                    },
                    formula.value
                )),
                Inline::AttributeReference { name, .. } => {
                    Some(format!("**attribute reference**  \nName: `{name}`"))
                }
                Inline::Passthrough { value, .. } => {
                    Some(format!("**passthrough**  \nLiteral content: `{value}`"))
                }
                Inline::Macro(node) => match node.kind {
                    adocweave::semantic::StandardMacroKind::Footnote => document
                        .catalogs()
                        .footnote_occurrence(node.range)
                        .map(|(footnote, _)| {
                            format!(
                                "**footnote {}**  \nID: `{}`  \nText: `{}`",
                                footnote.number,
                                footnote.id.as_deref().unwrap_or("anonymous"),
                                footnote.text
                            )
                        }),
                    adocweave::semantic::StandardMacroKind::BibliographyAnchor => document
                        .catalogs()
                        .bibliography()
                        .iter()
                        .find(|entry| entry.definition_range == node.range)
                        .map(|entry| {
                            format!(
                                "**bibliography entry**  \nID: `{}`  \nReferences: {}",
                                entry.id,
                                entry.references.len()
                            )
                        }),
                    adocweave::semantic::StandardMacroKind::IndexTerm => document
                        .catalogs()
                        .index()
                        .iter()
                        .find(|entry| entry.occurrences.contains(&node.range))
                        .map(|entry| {
                            format!("**index term**  \nPath: `{}`", entry.terms.join(" > "))
                        }),
                    _ => Some(format!(
                        "**{:?} macro**  \nTarget: `{}`",
                        node.kind, node.target
                    )),
                },
                Inline::Text(_)
                | Inline::Literal { .. }
                | Inline::Styled { .. }
                | Inline::HardBreak { .. } => None,
            };
            if let Some(value) = value {
                found = Some((value, inline.range()));
            }
        }
    });
    found
}

fn block_presentation_hover(
    document: &adocweave::semantic::Document,
    offset: u32,
) -> Option<(String, TextRange)> {
    let mut found = None;
    adocweave::semantic::walk(document, |node| {
        let adocweave::semantic::SemanticNode::Block(block) = node else {
            return;
        };
        match block {
            parser::Block::Paragraph(value)
                if value
                    .admonition
                    .as_ref()
                    .is_some_and(|item| range_contains_offset(item.label_range, offset)) =>
            {
                let item = value.admonition.as_ref().expect("guarded admonition");
                found = Some((
                    format!("**{} admonition**", item.kind.label()),
                    item.label_range,
                ));
            }
            parser::Block::Delimited(value) => match &value.presentation {
                Some(parser::DelimitedPresentation::Admonition(item))
                    if range_contains_offset(item.label_range, offset) =>
                {
                    found = Some((
                        format!("**{} admonition**", item.kind.label()),
                        item.label_range,
                    ));
                }
                Some(parser::DelimitedPresentation::Quote(item))
                    if range_contains_offset(
                        value.metadata.range.unwrap_or(value.range),
                        offset,
                    ) =>
                {
                    let kind = match item.kind {
                        parser::QuoteKind::Quote => "quote",
                        parser::QuoteKind::Verse => "verse",
                    };
                    found = Some((
                        format!(
                            "**{kind} block**  \nAttribution: `{}`  \nCitation: `{}`",
                            item.attribution.as_ref().map_or("", |value| &value.value),
                            item.citation.as_ref().map_or("", |value| &value.value)
                        ),
                        value.metadata.range.unwrap_or(value.range),
                    ));
                }
                _ => {}
            },
            _ => {}
        }
    });
    found
}

#[cfg(test)]
mod tests {
    use adocweave::{Analysis, AnalysisOptions, AnalysisRequest, NeverCancel};
    use async_lsp::lsp_types as lsp;

    use super::{HoverPresentation, hover};
    use crate::PositionEncoding;

    fn analyze(source: &str) -> Analysis {
        AnalysisRequest::new(None, 1, 1, source, AnalysisOptions::default())
            .analyze(&NeverCancel)
            .expect("analysis")
            .analysis
    }

    fn uri() -> lsp::Url {
        "file:///hover.adoc".parse().expect("valid URI")
    }

    fn project(
        source: &str,
        needle: &str,
        encoding: PositionEncoding,
        presentation: HoverPresentation,
    ) -> Option<lsp::Hover> {
        let analysis = analyze(source);
        let offset = u32::try_from(source.find(needle).expect("needle")).expect("source offset");
        hover(
            &analysis,
            &uri(),
            offset,
            std::iter::empty(),
            encoding,
            presentation,
        )
        .expect("hover projection")
    }

    fn markdown_value(hover: &lsp::Hover) -> &str {
        match &hover.contents {
            lsp::HoverContents::Markup(content) => &content.value,
            _ => panic!("expected markup hover"),
        }
    }

    #[test]
    fn attributes_references_links_and_math_use_analysis_facts() {
        let source = ":name: value\n\n{name}\n\nSee <<target>> and https://example.com[label].\n\n[#target]\n== Target\n\nstem:[x+y]\n";

        let attribute = project(
            source,
            "name:",
            PositionEncoding::Utf16,
            HoverPresentation::Markdown,
        )
        .expect("attribute hover");
        assert!(markdown_value(&attribute).contains("**document attribute**"));
        assert!(markdown_value(&attribute).contains("Folded value"));

        let reference = project(
            source,
            "{name}",
            PositionEncoding::Utf16,
            HoverPresentation::Markdown,
        )
        .expect("attribute reference hover");
        assert!(markdown_value(&reference).contains("**attribute reference**"));
        assert!(markdown_value(&reference).contains("Value: `value`"));

        let cross_reference = project(
            source,
            "<<target",
            PositionEncoding::Utf16,
            HoverPresentation::Markdown,
        )
        .expect("cross-reference hover");
        assert!(markdown_value(&cross_reference).contains("**cross reference**"));
        assert!(markdown_value(&cross_reference).contains("Target: `target`"));

        let link = project(
            source,
            "https://",
            PositionEncoding::Utf16,
            HoverPresentation::Markdown,
        )
        .expect("link hover");
        assert!(markdown_value(&link).contains("**external link**"));
        assert!(markdown_value(&link).contains("https://example.com"));

        let formula = project(
            source,
            "x+y",
            PositionEncoding::Utf16,
            HoverPresentation::Markdown,
        )
        .expect("formula hover");
        assert!(markdown_value(&formula).contains("**LaTeX formula**"));
        assert!(markdown_value(&formula).contains("Content: `x+y`"));
    }

    #[test]
    fn unicode_ranges_follow_the_negotiated_encoding() {
        let source = "https://example.com[例😀]";
        let utf8 = project(
            source,
            "example",
            PositionEncoding::Utf8,
            HoverPresentation::Markdown,
        )
        .expect("UTF-8 hover");
        let utf16 = project(
            source,
            "example",
            PositionEncoding::Utf16,
            HoverPresentation::Markdown,
        )
        .expect("UTF-16 hover");

        let utf8_range = utf8.range.expect("UTF-8 range");
        let utf16_range = utf16.range.expect("UTF-16 range");
        assert_eq!(utf8_range.start, lsp::Position::new(0, 0));
        assert_eq!(
            utf8_range.end.character,
            u32::try_from(source.len()).expect("UTF-8 length")
        );
        assert_eq!(utf16_range.start, lsp::Position::new(0, 0));
        assert_eq!(
            utf16_range.end.character,
            u32::try_from(source.encode_utf16().count()).expect("UTF-16 length")
        );
    }

    #[test]
    fn presentation_policy_only_changes_the_hover_container() {
        let source = "= Heading";
        let markdown = project(
            source,
            "Heading",
            PositionEncoding::Utf16,
            HoverPresentation::Markdown,
        )
        .expect("markdown hover");
        let plain = project(
            source,
            "Heading",
            PositionEncoding::Utf16,
            HoverPresentation::PlainText,
        )
        .expect("plain-text hover");
        let legacy = project(
            source,
            "Heading",
            PositionEncoding::Utf16,
            HoverPresentation::Legacy,
        )
        .expect("legacy hover");

        assert!(markdown_value(&markdown).contains("**document title**"));
        match plain.contents {
            lsp::HoverContents::Markup(content) => {
                assert_eq!(content.kind, lsp::MarkupKind::PlainText);
                assert!(!content.value.contains("**"));
                assert!(!content.value.contains('`'));
            }
            _ => panic!("expected plain-text markup"),
        }
        match legacy.contents {
            lsp::HoverContents::Scalar(lsp::MarkedString::String(value)) => {
                assert!(!value.contains("**"));
                assert!(!value.contains('`'));
            }
            _ => panic!("expected legacy scalar"),
        }
    }

    #[test]
    fn unrelated_text_has_no_hover() {
        assert!(
            project(
                "ordinary text",
                "ordinary",
                PositionEncoding::Utf16,
                HoverPresentation::Markdown,
            )
            .is_none()
        );
    }
}
