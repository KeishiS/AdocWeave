//! Deterministic, host-independent projections derived from one [`Analysis`].

use std::fmt::Write as _;

use crate::core::{Analysis, SourceId};
use crate::document::{ReferenceTarget, ReferenceTargetKind};
use crate::inline::{Inline, Link};
use crate::parser::{AstBlock, ListBlock};
use crate::reference::{ReferenceKey, ResolutionOutcome, ResolvedReference};
use crate::source::TextRange;

pub const PROJECTION_CONTRACT_VERSION: u16 = 2;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentProjection {
    pub contract_version: u16,
    pub source_id: Option<SourceId>,
    pub title: Option<ProjectedText>,
    pub targets: Vec<ReferenceTarget>,
    pub external_links: Vec<ExternalLink>,
    pub reference_edges: Vec<ReferenceEdge>,
    pub searchable_text: SearchableText,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectedText {
    pub source_range: TextRange,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalLink {
    pub source_range: TextRange,
    pub target_range: TextRange,
    pub target: String,
    pub label: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferenceEdge {
    pub source_id: Option<SourceId>,
    pub source_range: TextRange,
    pub target: ReferenceKey,
    pub resolution: Option<ResolutionOutcome>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchTextKind {
    Prose,
    Code,
}

impl SearchTextKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Prose => "prose",
            Self::Code => "code",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchTextSegment {
    pub kind: SearchTextKind,
    pub source_range: TextRange,
    pub text: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SearchableText {
    pub text: String,
    pub segments: Vec<SearchTextSegment>,
}

pub fn project(analysis: &Analysis, resolutions: &[ResolvedReference]) -> DocumentProjection {
    let title = analysis
        .ast()
        .blocks()
        .iter()
        .find_map(|block| match block {
            AstBlock::Heading(heading)
                if matches!(heading.kind, crate::parser::HeadingKind::DocumentTitle) =>
            {
                Some(ProjectedText {
                    source_range: heading.text_range,
                    text: inline_text(&heading.inlines),
                })
            }
            _ => None,
        });

    let mut external_links = Vec::new();
    analysis
        .ast()
        .visit_inline_sequences(|inlines| collect_links(inlines, &mut external_links));
    external_links.sort_by_key(|link| (link.source_range.start(), link.source_range.end()));

    let reference_edges = analysis
        .references()
        .iter()
        .filter_map(|reference| {
            let target = ReferenceKey::from_destination(&reference.destination)?;
            let resolution = resolutions
                .iter()
                .find(|resolution| resolution.source_range == reference.range)
                .map(|resolution| resolution.outcome.clone());
            Some(ReferenceEdge {
                source_id: analysis.source_id().cloned(),
                source_range: reference.range,
                target,
                resolution,
            })
        })
        .collect();

    DocumentProjection {
        contract_version: PROJECTION_CONTRACT_VERSION,
        source_id: analysis.source_id().cloned(),
        title,
        targets: analysis.reference_targets().to_vec(),
        external_links,
        reference_edges,
        searchable_text: searchable_text(analysis),
    }
}

pub fn searchable_text(analysis: &Analysis) -> SearchableText {
    let mut segments = Vec::new();
    collect_search_blocks(&analysis.ast().blocks(), &mut segments);
    let text = segments
        .iter()
        .map(|segment| segment.text.as_str())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    SearchableText { text, segments }
}

fn collect_links(inlines: &[Inline], output: &mut Vec<ExternalLink>) {
    for inline in inlines {
        match inline {
            Inline::Link(link) => {
                output.push(project_link(link));
                collect_links(&link.label, output);
            }
            Inline::Reference(reference) => collect_links(&reference.label, output),
            Inline::Styled { children, .. } => collect_links(children, output),
            Inline::Text(_)
            | Inline::Literal { .. }
            | Inline::AttributeReference { .. }
            | Inline::Formula(_) => {}
        }
    }
}

fn project_link(link: &Link) -> ExternalLink {
    let label = inline_text(&link.label);
    ExternalLink {
        source_range: link.range,
        target_range: link.target_range,
        target: link.target.clone(),
        label: if label.is_empty() {
            link.target.clone()
        } else {
            label
        },
    }
}

fn collect_search_blocks(blocks: &[AstBlock], output: &mut Vec<SearchTextSegment>) {
    for block in blocks {
        match block {
            AstBlock::Heading(heading) => push_search(
                output,
                SearchTextKind::Prose,
                heading.text_range,
                inline_text(&heading.inlines),
            ),
            AstBlock::Paragraph(paragraph) => {
                push_search(
                    output,
                    SearchTextKind::Prose,
                    paragraph.content_range,
                    fold_line_endings(&inline_text(&paragraph.inlines)),
                );
            }
            AstBlock::Literal(literal) => push_search(
                output,
                SearchTextKind::Code,
                literal.content_range,
                literal.value.clone(),
            ),
            AstBlock::Source(source) => push_search(
                output,
                SearchTextKind::Code,
                source.content_range,
                source.value.clone(),
            ),
            AstBlock::List(list) => collect_search_list(list, output),
            AstBlock::Math(_) | AstBlock::Unsupported(_) => {}
        }
    }
}

fn collect_search_list(list: &ListBlock, output: &mut Vec<SearchTextSegment>) {
    for item in &list.items {
        push_search(
            output,
            SearchTextKind::Prose,
            item.text_range,
            inline_text(&item.inlines),
        );
        for child in &item.children {
            collect_search_list(child, output);
        }
        collect_search_blocks(&item.continuations, output);
    }
}

fn push_search(
    output: &mut Vec<SearchTextSegment>,
    kind: SearchTextKind,
    source_range: TextRange,
    text: String,
) {
    let text = text.trim_end_matches(['\r', '\n']).to_owned();
    if !text.is_empty() {
        output.push(SearchTextSegment {
            kind,
            source_range,
            text,
        });
    }
}

fn inline_text(inlines: &[Inline]) -> String {
    let mut output = String::new();
    for inline in inlines {
        match inline {
            Inline::Text(text) => output.push_str(&text.value),
            Inline::Literal { value, .. } => output.push_str(value),
            Inline::Styled { children, .. } => output.push_str(&inline_text(children)),
            Inline::AttributeReference { .. } | Inline::Formula(_) => {}
            Inline::Link(link) => {
                let label = inline_text(&link.label);
                output.push_str(if label.is_empty() {
                    &link.target
                } else {
                    &label
                });
            }
            Inline::Reference(reference) => {
                let label = inline_text(&reference.label);
                output.push_str(if label.is_empty() {
                    &reference.target_source
                } else {
                    &label
                });
            }
        }
    }
    output
}

fn fold_line_endings(value: &str) -> String {
    value
        .lines()
        .map(|line| line.trim_end_matches([' ', '\t']))
        .collect::<Vec<_>>()
        .join(" ")
}

impl DocumentProjection {
    /// Stable JSON without relying on a host serialization framework.
    pub fn render_json(&self) -> String {
        let mut output = String::new();
        write!(
            output,
            "{{\"contractVersion\":{},\"sourceId\":",
            self.contract_version
        )
        .expect("writing to String cannot fail");
        write_optional_string(&mut output, self.source_id.as_ref().map(SourceId::as_str));
        output.push_str(",\"title\":");
        match &self.title {
            Some(title) => write_projected_text(&mut output, title),
            None => output.push_str("null"),
        }
        output.push_str(",\"targets\":[");
        for (index, target) in self.targets.iter().enumerate() {
            if index > 0 {
                output.push(',');
            }
            write!(
                output,
                "{{\"kind\":\"{}\",\"id\":{},\"label\":{},\"idRange\":{},\"targetRange\":{}}}",
                reference_target_kind(target.kind),
                json_string(&target.id),
                json_string(&target.label),
                json_range(target.id_range),
                json_range(target.target_range)
            )
            .expect("writing to String cannot fail");
        }
        output.push_str("],\"externalLinks\":[");
        for (index, link) in self.external_links.iter().enumerate() {
            if index > 0 {
                output.push(',');
            }
            write!(
                output,
                "{{\"sourceRange\":{},\"targetRange\":{},\"target\":{},\"label\":{}}}",
                json_range(link.source_range),
                json_range(link.target_range),
                json_string(&link.target),
                json_string(&link.label)
            )
            .expect("writing to String cannot fail");
        }
        output.push_str("],\"referenceEdges\":[");
        for (index, edge) in self.reference_edges.iter().enumerate() {
            if index > 0 {
                output.push(',');
            }
            write_reference_edge(&mut output, edge);
        }
        output.push_str("],\"searchableText\":{\"text\":");
        output.push_str(&json_string(&self.searchable_text.text));
        output.push_str(",\"segments\":[");
        for (index, segment) in self.searchable_text.segments.iter().enumerate() {
            if index > 0 {
                output.push(',');
            }
            write!(
                output,
                "{{\"kind\":\"{}\",\"sourceRange\":{},\"text\":{}}}",
                segment.kind.as_str(),
                json_range(segment.source_range),
                json_string(&segment.text)
            )
            .expect("writing to String cannot fail");
        }
        output.push_str("]}}");
        output
    }
}

fn write_projected_text(output: &mut String, text: &ProjectedText) {
    write!(
        output,
        "{{\"sourceRange\":{},\"text\":{}}}",
        json_range(text.source_range),
        json_string(&text.text)
    )
    .expect("writing to String cannot fail");
}

fn write_reference_edge(output: &mut String, edge: &ReferenceEdge) {
    output.push_str("{\"sourceId\":");
    write_optional_string(output, edge.source_id.as_ref().map(SourceId::as_str));
    write!(
        output,
        ",\"sourceRange\":{},\"target\":{}",
        json_range(edge.source_range),
        reference_key_json(&edge.target)
    )
    .expect("writing to String cannot fail");
    output.push_str(",\"resolution\":");
    match &edge.resolution {
        Some(ResolutionOutcome::Resolved { href }) => {
            write!(
                output,
                "{{\"status\":\"resolved\",\"href\":{}}}",
                json_string(href)
            )
            .expect("writing to String cannot fail");
        }
        Some(ResolutionOutcome::Failed(failure)) => {
            write!(
                output,
                "{{\"status\":\"failed\",\"kind\":\"{}\",\"message\":{}}}",
                failure.kind.diagnostic_code(),
                json_string(&failure.message)
            )
            .expect("writing to String cannot fail");
        }
        None => output.push_str("null"),
    }
    output.push('}');
}

fn reference_key_json(key: &ReferenceKey) -> String {
    match key {
        ReferenceKey::Local { anchor } => {
            format!("{{\"kind\":\"local\",\"anchor\":{}}}", json_string(anchor))
        }
        ReferenceKey::Document { document, anchor } => format!(
            "{{\"kind\":\"document\",\"document\":{},\"anchor\":{}}}",
            json_string(document),
            optional_string_json(anchor.as_deref())
        ),
        ReferenceKey::Scheme {
            scheme,
            locator,
            anchor,
        } => format!(
            "{{\"kind\":\"scheme\",\"scheme\":{},\"locator\":{},\"anchor\":{}}}",
            json_string(scheme),
            json_string(locator),
            optional_string_json(anchor.as_deref())
        ),
    }
}

const fn reference_target_kind(kind: ReferenceTargetKind) -> &'static str {
    match kind {
        ReferenceTargetKind::DocumentTitle => "document-title",
        ReferenceTargetKind::Section => "section",
        ReferenceTargetKind::ExplicitAnchor => "explicit-anchor",
    }
}

fn write_optional_string(output: &mut String, value: Option<&str>) {
    output.push_str(&optional_string_json(value));
}

fn optional_string_json(value: Option<&str>) -> String {
    value.map_or_else(|| "null".to_owned(), json_string)
}

fn json_range(range: TextRange) -> String {
    format!(
        "{{\"start\":{},\"end\":{}}}",
        range.start().to_u32(),
        range.end().to_u32()
    )
}

fn json_string(value: &str) -> String {
    crate::json::string(value)
}

#[cfg(test)]
mod tests {
    use crate::{Engine, ParseOptions, SourceId};

    use super::*;

    #[test]
    fn projections_are_stable_and_keep_links_and_reference_kinds_distinct() {
        let source = "= Title\n\n[[part]]\n== Section\n\nhttps://example.com[Site] <<part>> xref:other.adoc#x[] xref:note:42[]\n\n[source,rust]\n----\nfn main() {}\n----\n\nstem:[x+y]\n";
        let analysis = Engine::new(ParseOptions {
            source_id: Some(SourceId::new("host:document")),
            ..ParseOptions::default()
        })
        .analyze(source)
        .expect("analysis");
        let projected = project(&analysis, &[]);
        let html = crate::html::render(&analysis.ast(), &crate::html::RenderPolicy::default());

        assert_eq!(projected.contract_version, PROJECTION_CONTRACT_VERSION);
        assert!(html.html.contains("<h1"));
        assert_eq!(projected.external_links.len(), 1);
        assert_eq!(projected.reference_edges.len(), 3);
        assert!(matches!(
            projected.reference_edges[0].target,
            ReferenceKey::Local { .. }
        ));
        assert!(matches!(
            projected.reference_edges[1].target,
            ReferenceKey::Document { .. }
        ));
        assert!(matches!(
            projected.reference_edges[2].target,
            ReferenceKey::Scheme { .. }
        ));
        assert!(projected.searchable_text.text.contains("fn main() {}"));
        assert!(!projected.searchable_text.text.contains("x+y"));
        assert_eq!(
            projected.render_json(),
            project(&analysis, &[]).render_json()
        );
    }

    #[test]
    fn reference_graph_attaches_optional_resolution_by_exact_source_range() {
        let analysis = Engine::new(ParseOptions::default())
            .analyze("xref:other.adoc[Other]")
            .expect("analysis");
        let resolution =
            ResolvedReference::resolved(analysis.references()[0].range, "https://example/other");
        let projected = project(&analysis, &[resolution]);

        assert!(matches!(
            projected.reference_edges[0].resolution,
            Some(ResolutionOutcome::Resolved { ref href })
                if href == "https://example/other"
        ));
    }

    #[test]
    fn projections_keep_the_version_two_json_contract() {
        let analysis = Engine::new(ParseOptions::default())
            .analyze("= T")
            .expect("analysis");
        assert_eq!(
            project(&analysis, &[]).render_json(),
            "{\"contractVersion\":2,\"sourceId\":null,\"title\":{\"sourceRange\":{\"start\":2,\"end\":3},\"text\":\"T\"},\"targets\":[{\"kind\":\"document-title\",\"id\":\"_t\",\"label\":\"T\",\"idRange\":{\"start\":2,\"end\":3},\"targetRange\":{\"start\":0,\"end\":3}}],\"externalLinks\":[],\"referenceEdges\":[],\"searchableText\":{\"text\":\"T\",\"segments\":[{\"kind\":\"prose\",\"sourceRange\":{\"start\":2,\"end\":3},\"text\":\"T\"}]}}"
        );
    }

    #[test]
    fn searchable_text_excludes_attributes_math_and_invisible_anchor_syntax() {
        let source = "= Visible\n:name: hidden\n\n[[secret]]\n== Section\n\nstem:[hidden-math]\n\n....\nvisible code\n....\n";
        let analysis = Engine::new(ParseOptions::default())
            .analyze(source)
            .expect("analysis");
        let searchable = searchable_text(&analysis);

        assert_eq!(searchable.text, "Visible\nSection\nvisible code");
        assert_eq!(
            searchable
                .segments
                .iter()
                .map(|segment| segment.kind)
                .collect::<Vec<_>>(),
            vec![
                SearchTextKind::Prose,
                SearchTextKind::Prose,
                SearchTextKind::Code
            ]
        );
    }
}
