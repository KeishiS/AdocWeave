//! Stateless completion presentation over selected document and workspace analyses.

use adocweave::Analysis;
use adocweave::semantic::{DocumentElement, document_element_at, source_language_candidates};
use async_lsp::lsp_types as lsp;

use crate::position::{PositionEncoding, cursor_touches_range, request_offset};
use crate::state::WorkspaceAnalysis;

pub(crate) fn completion(
    analysis: &Analysis,
    workspaces: &[&WorkspaceAnalysis],
    uri: &lsp::Url,
    position: lsp::Position,
    encoding: PositionEncoding,
) -> Result<lsp::CompletionResponse, String> {
    let offset = request_offset(analysis.source_document(), position, encoding)?;
    if attribute_completion_context(analysis.source(), offset as usize) {
        let values = workspaces
            .iter()
            .find_map(|workspace| {
                expanded_offset_for_origin(workspace, uri, offset).map(|expanded| {
                    workspace
                        .analysis
                        .attribute_environment()
                        .values_at(expanded)
                })
            })
            .unwrap_or_else(|| {
                analysis
                    .attribute_environment()
                    .values_at(adocweave::text::TextSize::new(offset as usize).expect("offset"))
            });
        return Ok(items(values.into_iter().map(|(name, value)| {
            lsp::CompletionItem {
                label: name,
                detail: Some(value),
                kind: Some(lsp::CompletionItemKind::VARIABLE),
                ..lsp::CompletionItem::default()
            }
        })));
    }
    if analysis
        .references()
        .iter()
        .any(|reference| cursor_touches_range(reference.target_range, offset))
    {
        return Ok(items(analysis.reference_targets().iter().map(|target| {
            lsp::CompletionItem {
                label: target.id.clone(),
                detail: Some(target.label.clone()),
                kind: Some(lsp::CompletionItemKind::REFERENCE),
                ..lsp::CompletionItem::default()
            }
        })));
    }
    let Some(element) = document_element_at(analysis.document(), offset) else {
        return Ok(empty_completion());
    };
    let metadata_candidates: Option<(&[&str], lsp::CompletionItemKind)> = match element {
        DocumentElement::MetadataRole(_) => {
            Some((&["lead", "discrete"], lsp::CompletionItemKind::VALUE))
        }
        DocumentElement::MetadataOption(_) => Some((
            &[
                "autowidth",
                "collapsible",
                "footer",
                "header",
                "interactive",
                "nowrap",
            ],
            lsp::CompletionItemKind::VALUE,
        )),
        DocumentElement::ElementAttribute(_) => Some((
            &[
                "CAUTION",
                "IMPORTANT",
                "NOTE",
                "TIP",
                "WARNING",
                "cols",
                "frame",
                "grid",
                "id",
                "options",
                "quote",
                "role",
                "stripes",
                "subs",
                "verse",
                "width",
            ],
            lsp::CompletionItemKind::PROPERTY,
        )),
        DocumentElement::MetadataTitle(_) | DocumentElement::MetadataId(_) => {
            return Ok(empty_completion());
        }
        _ => None,
    };
    if let Some((candidates, kind)) = metadata_candidates {
        return Ok(items(candidates.iter().map(|candidate| {
            lsp::CompletionItem {
                label: (*candidate).to_owned(),
                kind: Some(kind),
                ..lsp::CompletionItem::default()
            }
        })));
    }
    let source = match element {
        DocumentElement::SourceLanguage(source) | DocumentElement::SourceAttribute(source) => {
            source
        }
        DocumentElement::HeadingMarker(_) | DocumentElement::HeadingText(_) => {
            return Ok(empty_completion());
        }
        DocumentElement::MetadataTitle(_)
        | DocumentElement::MetadataId(_)
        | DocumentElement::MetadataRole(_)
        | DocumentElement::MetadataOption(_)
        | DocumentElement::ElementAttribute(_) => unreachable!(),
    };
    let offset = offset as usize;
    let text = analysis.source();
    let attribute_start = source.attribute_range.start().to_usize();
    if offset > text.len() || !text[attribute_start..offset].contains(',') {
        return Ok(empty_completion());
    }
    let prefix = source
        .language_range
        .and_then(|range| {
            let start = range.start().to_usize();
            (start <= offset).then(|| &text[start..offset])
        })
        .unwrap_or("");
    Ok(items(source_language_candidates(prefix).iter().map(
        |language| lsp::CompletionItem {
            label: language.to_string(),
            kind: Some(lsp::CompletionItemKind::VALUE),
            ..lsp::CompletionItem::default()
        },
    )))
}

pub(crate) fn empty_completion() -> lsp::CompletionResponse {
    lsp::CompletionResponse::Array(Vec::new())
}

fn items(items: impl IntoIterator<Item = lsp::CompletionItem>) -> lsp::CompletionResponse {
    lsp::CompletionResponse::Array(items.into_iter().collect())
}

fn expanded_offset_for_origin(
    workspace: &WorkspaceAnalysis,
    uri: &lsp::Url,
    offset: u32,
) -> Option<adocweave::text::TextSize> {
    workspace.document.source_map().iter().find_map(|segment| {
        if segment.mapping != adocweave::preprocess::SourceMapping::Identity
            || segment
                .origin
                .source_id
                .as_ref()
                .is_none_or(|source_id| source_id.as_str() != uri.as_str())
        {
            return None;
        }
        let origin = segment.origin.range.text_range();
        if !(origin.start().to_u32() <= offset && offset <= origin.end().to_u32()) {
            return None;
        }
        let relative = offset.checked_sub(origin.start().to_u32())?;
        adocweave::text::TextSize::new(segment.output_range.start().to_usize() + relative as usize)
            .ok()
    })
}

fn attribute_completion_context(source: &str, offset: usize) -> bool {
    if offset > source.len() || !source.is_char_boundary(offset) {
        return false;
    }
    let line_start = source[..offset].rfind('\n').map_or(0, |index| index + 1);
    let bytes = &source.as_bytes()[line_start..offset];
    let mut open = None;
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' && bytes.get(index + 1) == Some(&b'{') {
            index += 2;
            continue;
        }
        match bytes[index] {
            b'{' => open = Some(index),
            b'}' => open = None,
            _ => {}
        }
        index += 1;
    }
    open.is_some_and(|open| {
        bytes[open + 1..]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    })
}

#[cfg(test)]
mod tests {
    use adocweave::{Analysis, AnalysisOptions, AnalysisRequest, NeverCancel};

    use super::*;

    fn analyze(source: &str) -> Analysis {
        AnalysisRequest::new(None, 1, 1, source, AnalysisOptions::default())
            .analyze(&NeverCancel)
            .expect("analysis")
            .analysis
    }

    fn labels(response: lsp::CompletionResponse) -> Vec<String> {
        let lsp::CompletionResponse::Array(items) = response else {
            panic!("completion array");
        };
        items.into_iter().map(|item| item.label).collect()
    }

    #[test]
    fn completion_presents_references_attributes_and_source_languages() {
        let uri = lsp::Url::parse("file:///a.adoc").expect("URI");
        let references = analyze("[[target]]\n== Target\n\n<<tar>>\n");
        assert!(
            labels(
                completion(
                    &references,
                    &[],
                    &uri,
                    lsp::Position::new(3, 4),
                    PositionEncoding::Utf16,
                )
                .expect("reference completion")
            )
            .contains(&"target".to_owned())
        );

        let attributes = analyze(":name: value\n\n{name}\n");
        assert!(
            labels(
                completion(
                    &attributes,
                    &[],
                    &uri,
                    lsp::Position::new(2, 3),
                    PositionEncoding::Utf16,
                )
                .expect("attribute completion")
            )
            .contains(&"name".to_owned())
        );

        let source = analyze("[source,r]\n----\n----\n");
        assert!(
            labels(
                completion(
                    &source,
                    &[],
                    &uri,
                    lsp::Position::new(0, 9),
                    PositionEncoding::Utf16,
                )
                .expect("source completion")
            )
            .iter()
            .any(|label| label.starts_with('r'))
        );
    }
}
