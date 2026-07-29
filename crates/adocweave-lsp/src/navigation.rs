//! Stateless navigation conversion over selected document and workspace analyses.

use adocweave::output::projection::project;
use adocweave::resolution::ReferenceKey;
use adocweave::text::SourceDocument;
use async_lsp::lsp_types as lsp;

use crate::position::{PositionEncoding, range_contains_offset, range_to_lsp, request_offset};
use crate::state::{DocumentSnapshot, WorkspaceAnalysis};

pub(crate) struct NavigationInput<'a> {
    pub document: &'a DocumentSnapshot,
    pub snapshots: &'a [DocumentSnapshot],
    pub workspaces: &'a [&'a WorkspaceAnalysis],
    pub encoding: PositionEncoding,
    pub source_document: &'a dyn Fn(&lsp::Url) -> Result<SourceDocument, String>,
}

pub(crate) enum Definition {
    Resolved(Option<lsp::GotoDefinitionResponse>),
    Host(ReferenceKey),
}

pub(crate) struct References {
    pub fallback: Vec<lsp::Location>,
    pub host_target: Option<ReferenceKey>,
}

pub(crate) struct DocumentLinks {
    links: Vec<lsp::DocumentLink>,
    pub unresolved: Vec<UnresolvedDocumentLink>,
}

pub(crate) struct UnresolvedDocumentLink {
    pub target: ReferenceKey,
    range: lsp::Range,
}

pub(crate) fn definition(
    input: &NavigationInput<'_>,
    uri: &lsp::Url,
    position: lsp::Position,
) -> Result<Definition, String> {
    let offset = request_offset(
        input.document.analysis.source_document(),
        position,
        input.encoding,
    )?;
    for workspace in input.workspaces {
        if let Some((reference, _)) = projected_attribute_reference_at(workspace, uri, offset)
            && let Some(binding_id) = reference.binding_id
            && let Some(binding) = workspace
                .projection
                .attribute_bindings
                .iter()
                .find(|binding| binding.value.id() == binding_id)
            && let Some(origin) = binding.name_origins.first()
        {
            return Ok(Definition::Resolved(Some(
                lsp::GotoDefinitionResponse::Scalar(attribute_origin_location(input, origin)?),
            )));
        }
    }
    if let Some(reference) = input
        .document
        .analysis
        .attribute_references()
        .iter()
        .find(|reference| range_contains_offset(reference.range, offset))
        && let Some(binding) = reference
            .binding_id
            .and_then(|id| input.document.analysis.attribute_environment().binding(id))
    {
        return Ok(Definition::Resolved(Some(
            lsp::GotoDefinitionResponse::Scalar(lsp::Location::new(
                uri.clone(),
                range_to_lsp(
                    binding.occurrence().name_range,
                    input.document.analysis.source_document(),
                    input.encoding,
                )?,
            )),
        )));
    }
    for workspace in input.workspaces {
        if let Some(directive) = workspace.projection.directives.iter().find(|directive| {
            directive
                .source_id
                .as_ref()
                .is_some_and(|source_id| source_id.as_str() == uri.as_str())
                && range_contains_offset(directive.target_range, offset)
        }) && let Some(target) = directive.resource_source_id.as_ref()
        {
            let target: lsp::Url = target
                .as_str()
                .parse()
                .map_err(|error| format!("invalid include resource URI: {error}"))?;
            return Ok(Definition::Resolved(Some(
                lsp::GotoDefinitionResponse::Scalar(lsp::Location::new(
                    target,
                    lsp::Range::default(),
                )),
            )));
        }
    }
    let Some(reference) = input
        .document
        .analysis
        .references()
        .iter()
        .find(|reference| range_contains_offset(reference.range, offset))
    else {
        return Ok(Definition::Resolved(None));
    };
    let Some(key) = reference.target.clone() else {
        return Ok(Definition::Resolved(None));
    };
    if let Some(identity) = reference_identity(uri, reference.target.as_ref())
        && let Some(location) = target_location(input, &identity.uri, identity.anchor.as_deref())?
    {
        return Ok(Definition::Resolved(Some(
            lsp::GotoDefinitionResponse::Scalar(location),
        )));
    }
    Ok(Definition::Host(key))
}

pub(crate) fn references(
    input: &NavigationInput<'_>,
    uri: &lsp::Url,
    position: lsp::Position,
    include_declaration: bool,
) -> Result<References, String> {
    let offset = request_offset(
        input.document.analysis.source_document(),
        position,
        input.encoding,
    )?;
    let projected_binding_origin = input.workspaces.iter().find_map(|workspace| {
        let binding_id = projected_attribute_reference_at(workspace, uri, offset)
            .and_then(|(reference, _)| reference.binding_id)
            .or_else(|| projected_attribute_binding_at(workspace, uri, offset))?;
        workspace
            .projection
            .attribute_bindings
            .iter()
            .find(|binding| binding.value.id() == binding_id)?
            .name_origins
            .first()
            .cloned()
    });
    if let Some(binding_origin) = projected_binding_origin {
        let mut locations = Vec::new();
        for workspace in input.workspaces {
            let Some(binding) = workspace
                .projection
                .attribute_bindings
                .iter()
                .find(|binding| {
                    binding
                        .name_origins
                        .iter()
                        .any(|origin| same_origin(origin, &binding_origin))
                })
            else {
                continue;
            };
            if include_declaration && let Some(origin) = binding.name_origins.first() {
                locations.push(attribute_origin_location(input, origin)?);
            }
            for reference in &workspace.projection.attribute_references {
                if reference.value.binding_id != Some(binding.value.id()) {
                    continue;
                }
                for origin in &reference.name_origins {
                    locations.push(attribute_origin_location(input, origin)?);
                }
            }
        }
        sort_and_dedup_locations(&mut locations);
        return Ok(References {
            fallback: locations,
            host_target: None,
        });
    }
    let local_binding_id = input
        .document
        .analysis
        .attribute_references()
        .iter()
        .find(|reference| range_contains_offset(reference.range, offset))
        .and_then(|reference| reference.binding_id)
        .or_else(|| {
            input
                .document
                .analysis
                .attribute_environment()
                .bindings()
                .iter()
                .find(|binding| range_contains_offset(binding.occurrence().name_range, offset))
                .map(adocweave::semantic::AttributeBinding::id)
        });
    if let Some(binding_id) = local_binding_id {
        let mut locations = Vec::new();
        if include_declaration
            && let Some(binding) = input
                .document
                .analysis
                .attribute_environment()
                .binding(binding_id)
        {
            locations.push(lsp::Location::new(
                uri.clone(),
                range_to_lsp(
                    binding.occurrence().name_range,
                    input.document.analysis.source_document(),
                    input.encoding,
                )?,
            ));
        }
        for reference in input.document.analysis.attribute_references() {
            if reference.binding_id == Some(binding_id) {
                locations.push(lsp::Location::new(
                    uri.clone(),
                    range_to_lsp(
                        reference.name_range,
                        input.document.analysis.source_document(),
                        input.encoding,
                    )?,
                ));
            }
        }
        return Ok(References {
            fallback: locations,
            host_target: None,
        });
    }
    let reference_at_position = input
        .document
        .analysis
        .references()
        .iter()
        .find(|reference| range_contains_offset(reference.range, offset));
    let key = reference_at_position
        .and_then(|reference| reference.target.clone())
        .or_else(|| {
            input
                .document
                .analysis
                .reference_targets()
                .iter()
                .find(|target| range_contains_offset(target.id_range, offset))
                .map(|target| ReferenceKey::Local {
                    anchor: target.id.clone(),
                })
        });
    let Some(key) = key else {
        return Ok(References {
            fallback: Vec::new(),
            host_target: None,
        });
    };
    let identity = reference_at_position
        .and_then(|reference| reference_identity(uri, reference.target.as_ref()))
        .or_else(|| match &key {
            ReferenceKey::Local { anchor } => Some(TargetIdentity {
                uri: uri.clone(),
                anchor: Some(anchor.clone()),
            }),
            ReferenceKey::Document { document, anchor } => {
                uri.join(document).ok().map(|uri| TargetIdentity {
                    uri,
                    anchor: anchor.clone(),
                })
            }
            ReferenceKey::Scheme { .. } => None,
        });
    let Some(identity) = identity else {
        return Ok(References {
            fallback: Vec::new(),
            host_target: Some(key),
        });
    };

    let mut locations = Vec::new();
    if include_declaration
        && let Some(location) = target_location(input, &identity.uri, identity.anchor.as_deref())?
    {
        locations.push(location);
    }
    for candidate in input.snapshots {
        let candidate_uri: lsp::Url = candidate
            .uri
            .parse()
            .map_err(|error| format!("invalid open document URI {}: {error}", candidate.uri))?;
        for reference in candidate.analysis.references() {
            if reference_identity(&candidate_uri, reference.target.as_ref()).as_ref()
                == Some(&identity)
            {
                locations.push(lsp::Location::new(
                    candidate_uri.clone(),
                    range_to_lsp(
                        reference.target_range,
                        candidate.analysis.source_document(),
                        input.encoding,
                    )?,
                ));
            }
        }
    }
    for workspace in input.workspaces {
        for reference in &workspace.projection.references {
            let Some(source_origin) = reference.origins.first() else {
                continue;
            };
            let Some(source_id) = &source_origin.source_id else {
                continue;
            };
            let source_uri: lsp::Url = source_id
                .as_str()
                .parse()
                .map_err(|error| format!("invalid projected reference URI: {error}"))?;
            if reference_identity(&source_uri, reference.value.target.as_ref()).as_ref()
                != Some(&identity)
            {
                continue;
            }
            let Some(target_origin) = reference
                .target_origins
                .iter()
                .find(|origin| origin.source_id.as_ref() == Some(source_id))
            else {
                continue;
            };
            let source_document = (input.source_document)(&source_uri)?;
            locations.push(lsp::Location::new(
                source_uri,
                range_to_lsp(
                    target_origin.range.text_range(),
                    &source_document,
                    input.encoding,
                )?,
            ));
        }
    }
    sort_and_dedup_locations(&mut locations);
    Ok(References {
        fallback: locations,
        host_target: Some(key),
    })
}

pub(crate) fn document_links(
    input: &NavigationInput<'_>,
    uri: &lsp::Url,
    tooltips: bool,
) -> Result<DocumentLinks, String> {
    let mut links = Vec::new();
    let mut unresolved = Vec::new();
    for link in project(
        &input.document.analysis,
        &adocweave::resolution::RenderInputs::default(),
    )
    .external_links
    {
        if !adocweave::resolution::AuthoredUrlPolicy::default().allows(&link.target) {
            continue;
        }
        let Ok(target) = lsp::Url::parse(&link.target) else {
            continue;
        };
        links.push(lsp::DocumentLink {
            range: range_to_lsp(
                link.target_range,
                input.document.analysis.source_document(),
                input.encoding,
            )?,
            target: Some(target),
            tooltip: tooltips.then(|| "外部リンクを開く".to_owned()),
            data: None,
        });
    }
    for reference in input.document.analysis.references() {
        let range = range_to_lsp(
            reference.target_range,
            input.document.analysis.source_document(),
            input.encoding,
        )?;
        if let Some(identity) = reference_identity(uri, reference.target.as_ref()) {
            let mut target = identity.uri;
            target.set_fragment(identity.anchor.as_deref());
            links.push(lsp::DocumentLink {
                range,
                target: Some(target),
                tooltip: tooltips.then(|| "参照先を開く".to_owned()),
                data: None,
            });
        } else if let Some(target) = reference.target.clone() {
            unresolved.push(UnresolvedDocumentLink { target, range });
        }
    }
    for workspace in input.workspaces {
        for directive in &workspace.projection.directives {
            if directive
                .source_id
                .as_ref()
                .is_none_or(|source_id| source_id.as_str() != uri.as_str())
            {
                continue;
            }
            let Some(target) = directive.resource_source_id.as_ref() else {
                continue;
            };
            let Ok(target) = target.as_str().parse() else {
                continue;
            };
            links.push(lsp::DocumentLink {
                range: range_to_lsp(
                    directive.target_range,
                    input.document.analysis.source_document(),
                    input.encoding,
                )?,
                target: Some(target),
                tooltip: tooltips.then(|| "include先を開く".to_owned()),
                data: None,
            });
        }
    }
    Ok(DocumentLinks { links, unresolved })
}

impl DocumentLinks {
    pub(crate) fn resolve(
        &mut self,
        unresolved: UnresolvedDocumentLink,
        location: Option<lsp::Location>,
        tooltips: bool,
    ) {
        if let Some(location) = location {
            self.links.push(lsp::DocumentLink {
                range: unresolved.range,
                target: Some(location.uri),
                tooltip: tooltips.then(|| "参照先を開く".to_owned()),
                data: None,
            });
        }
    }

    pub(crate) fn finish(mut self) -> Vec<lsp::DocumentLink> {
        self.links.sort_by_key(|link| {
            (
                link.range.start.line,
                link.range.start.character,
                link.range.end.line,
                link.range.end.character,
            )
        });
        self.links
            .dedup_by(|left, right| left.range == right.range && left.target == right.target);
        self.links
    }
}

fn target_location(
    input: &NavigationInput<'_>,
    uri: &lsp::Url,
    anchor: Option<&str>,
) -> Result<Option<lsp::Location>, String> {
    let Some(document) = input
        .snapshots
        .iter()
        .find(|document| document.uri == uri.as_str())
    else {
        return Ok(None);
    };
    let target = anchor
        .and_then(|anchor| {
            document
                .analysis
                .reference_targets()
                .iter()
                .find(|target| target.id == anchor)
        })
        .or_else(|| document.analysis.reference_targets().first());
    let Some(target) = target else {
        return Ok(None);
    };
    Ok(Some(lsp::Location::new(
        uri.clone(),
        range_to_lsp(
            target.target_range,
            document.analysis.source_document(),
            input.encoding,
        )?,
    )))
}

fn attribute_origin_location(
    input: &NavigationInput<'_>,
    origin: &adocweave::preprocess::SourceOrigin,
) -> Result<lsp::Location, String> {
    let source_id = origin
        .source_id
        .as_ref()
        .ok_or_else(|| "attribute origin has no source ID".to_owned())?;
    let uri: lsp::Url = source_id
        .as_str()
        .parse()
        .map_err(|error| format!("invalid attribute origin URI: {error}"))?;
    let source_document = (input.source_document)(&uri)?;
    Ok(lsp::Location::new(
        uri,
        range_to_lsp(origin.range.text_range(), &source_document, input.encoding)?,
    ))
}

fn projected_attribute_reference_at<'a>(
    workspace: &'a WorkspaceAnalysis,
    uri: &lsp::Url,
    offset: u32,
) -> Option<(
    &'a adocweave::semantic::AttributeReference,
    &'a adocweave::preprocess::SourceOrigin,
)> {
    workspace
        .projection
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

fn projected_attribute_binding_at(
    workspace: &WorkspaceAnalysis,
    uri: &lsp::Url,
    offset: u32,
) -> Option<adocweave::semantic::AttributeBindingId> {
    workspace
        .projection
        .attribute_bindings
        .iter()
        .find(|binding| {
            binding.name_origins.iter().any(|origin| {
                origin
                    .source_id
                    .as_ref()
                    .is_some_and(|source_id| source_id.as_str() == uri.as_str())
                    && range_contains_offset(origin.range.text_range(), offset)
            })
        })
        .map(|binding| binding.value.id())
}

fn same_origin(
    left: &adocweave::preprocess::SourceOrigin,
    right: &adocweave::preprocess::SourceOrigin,
) -> bool {
    left.source_id == right.source_id && left.range == right.range
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TargetIdentity {
    uri: lsp::Url,
    anchor: Option<String>,
}

fn reference_identity(
    source_uri: &lsp::Url,
    destination: Option<&ReferenceKey>,
) -> Option<TargetIdentity> {
    match destination {
        Some(ReferenceKey::Local { anchor }) => Some(TargetIdentity {
            uri: source_uri.clone(),
            anchor: Some(anchor.clone()),
        }),
        Some(ReferenceKey::Document { document, anchor }) => {
            source_uri.join(document).ok().map(|uri| TargetIdentity {
                uri,
                anchor: anchor.clone(),
            })
        }
        Some(ReferenceKey::Scheme { .. }) | None => None,
    }
}

fn sort_and_dedup_locations(locations: &mut Vec<lsp::Location>) {
    locations.sort_by(|left, right| {
        (
            left.uri.as_str(),
            left.range.start.line,
            left.range.start.character,
            left.range.end.line,
            left.range.end.character,
        )
            .cmp(&(
                right.uri.as_str(),
                right.range.start.line,
                right.range.start.character,
                right.range.end.line,
                right.range.end.character,
            ))
    });
    locations.dedup();
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use adocweave::{AnalysisOptions, AnalysisRequest, NeverCancel, SourceId};

    use super::*;

    fn snapshot(uri: &str, source: &str) -> DocumentSnapshot {
        let request = AnalysisRequest::new(
            Some(SourceId::new(uri)),
            1,
            1,
            source,
            AnalysisOptions::default(),
        );
        let result = request.analyze(&NeverCancel).expect("analysis");
        DocumentSnapshot {
            uri: uri.to_owned(),
            revision: result.revision,
            analysis: Arc::new(result.analysis),
            format: adocweave::output::formatter::FormatConfig::default(),
        }
    }

    fn no_projected_source(uri: &lsp::Url) -> Result<SourceDocument, String> {
        Err(format!("unexpected projected source: {uri}"))
    }

    fn location(uri: &str, line: u32, character: u32) -> lsp::Location {
        lsp::Location::new(
            lsp::Url::parse(uri).expect("URI"),
            lsp::Range::new(
                lsp::Position::new(line, character),
                lsp::Position::new(line, character + 1),
            ),
        )
    }

    #[test]
    fn reference_identity_separates_local_document_and_scheme_targets() {
        let source = lsp::Url::parse("file:///book/a.adoc").expect("URI");
        assert_eq!(
            reference_identity(
                &source,
                Some(&ReferenceKey::Local {
                    anchor: "local".to_owned(),
                })
            ),
            Some(TargetIdentity {
                uri: source.clone(),
                anchor: Some("local".to_owned()),
            })
        );
        assert_eq!(
            reference_identity(
                &source,
                Some(&ReferenceKey::Document {
                    document: "b.adoc".to_owned(),
                    anchor: Some("other".to_owned()),
                })
            ),
            Some(TargetIdentity {
                uri: lsp::Url::parse("file:///book/b.adoc").expect("URI"),
                anchor: Some("other".to_owned()),
            })
        );
        assert!(
            reference_identity(
                &source,
                Some(&ReferenceKey::Scheme {
                    scheme: "note".to_owned(),
                    locator: "42".to_owned(),
                    anchor: None,
                })
            )
            .is_none()
        );
    }

    #[test]
    fn locations_have_stable_order_and_remove_duplicates() {
        let first = location("file:///a.adoc", 1, 2);
        let second = location("file:///b.adoc", 0, 0);
        let mut locations = vec![second.clone(), first.clone(), first.clone()];
        sort_and_dedup_locations(&mut locations);
        assert_eq!(locations, vec![first, second]);
    }

    #[test]
    fn definition_resolves_local_and_document_targets_from_selected_snapshots() {
        let first = snapshot(
            "file:///book/a.adoc",
            "[[local]]\n== Local\n\n<<local>> xref:b.adoc#other[B]\n",
        );
        let second = snapshot("file:///book/b.adoc", "[[other]]\n== Other\n");
        let snapshots = vec![first.clone(), second];
        let input = NavigationInput {
            document: &first,
            snapshots: &snapshots,
            workspaces: &[],
            encoding: PositionEncoding::Utf16,
            source_document: &no_projected_source,
        };
        let source = lsp::Url::parse(&first.uri).expect("URI");

        let Definition::Resolved(Some(lsp::GotoDefinitionResponse::Scalar(local))) =
            definition(&input, &source, lsp::Position::new(3, 3)).expect("local definition")
        else {
            panic!("local scalar definition");
        };
        assert_eq!(local.uri, source);
        let Definition::Resolved(Some(lsp::GotoDefinitionResponse::Scalar(document))) =
            definition(&input, &source, lsp::Position::new(3, 22)).expect("document definition")
        else {
            panic!("document scalar definition");
        };
        assert_eq!(document.uri.as_str(), "file:///book/b.adoc");
    }

    #[test]
    fn reference_ranges_follow_utf8_and_utf16_encoding() {
        let document = snapshot("file:///unicode.adoc", "[[節😀]]\n== 見出し\n\n<<節😀>>\n");
        let snapshots = vec![document.clone()];
        let uri = lsp::Url::parse(&document.uri).expect("URI");
        for (encoding, expected_end) in [(PositionEncoding::Utf8, 9), (PositionEncoding::Utf16, 5)]
        {
            let input = NavigationInput {
                document: &document,
                snapshots: &snapshots,
                workspaces: &[],
                encoding,
                source_document: &no_projected_source,
            };
            let result =
                references(&input, &uri, lsp::Position::new(0, 2), false).expect("references");
            assert_eq!(result.fallback.len(), 1);
            assert_eq!(result.fallback[0].range.start.character, 2);
            assert_eq!(result.fallback[0].range.end.character, expected_end);
        }
    }

    #[test]
    fn document_links_keep_deterministic_order_when_host_resolution_fails() {
        let direct = lsp::DocumentLink {
            range: lsp::Range::new(lsp::Position::new(2, 0), lsp::Position::new(2, 3)),
            target: Some(lsp::Url::parse("file:///direct.adoc").expect("URI")),
            tooltip: None,
            data: None,
        };
        let unresolved = UnresolvedDocumentLink {
            target: ReferenceKey::Local {
                anchor: "unused".to_owned(),
            },
            range: lsp::Range::new(lsp::Position::new(0, 0), lsp::Position::new(0, 3)),
        };
        let mut links = DocumentLinks {
            links: vec![direct.clone(), direct.clone()],
            unresolved: Vec::new(),
        };
        links.resolve(unresolved, None, false);
        assert_eq!(links.finish(), vec![direct]);
    }
}
