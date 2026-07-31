//! Typed inline render planning and deterministic body serialization.

use crate::inline::{Inline, InlineLiteralKind, InlineStyle, Link, Reference};
use crate::parser::{AstBlock, AstDocument, Heading, HeadingKind};
use crate::resource::MediaFamily;
use crate::url::UrlProvenance;

use super::plan::{self, PlannedReferenceHref};
use super::safe::{
    ActiveUrlAttributeName, AttributeValue, BooleanAttributeName, ClassName, ElementName,
    HtmlWriter, OwnedSafeFragmentUrl, OwnedSafeUrl, PassiveAttributeName, SafeFragmentUrl,
    SourceLanguageClass, TextValue,
};
use super::{
    InlineRenderContext, RenderPolicy, UnresolvedReferencePresentation, append_plan_diagnostics,
    bibliography_reference_id, render_diagnostic,
};

pub(super) struct BodyTraversalPlan<'document> {
    pub(super) steps: Vec<BodyTraversalStep<'document>>,
}

pub(super) enum BodyTraversalStep<'document> {
    TableOfContents,
    FootnoteCatalog,
    Block {
        block: &'document AstBlock,
        scope: RenderScope,
        render_header_metadata: bool,
    },
}

#[derive(Clone, Copy, Default)]
pub(super) struct RenderScope {
    pub(super) bibliography_section: bool,
}

impl RenderScope {
    fn enter(self, scope: crate::presentation::LayoutScope) -> Self {
        Self {
            bibliography_section: self.bibliography_section
                || matches!(scope, crate::presentation::LayoutScope::Bibliography),
        }
    }
}

pub(super) fn plan_body_traversal<'document>(
    document: &'document AstDocument,
    policy: &RenderPolicy,
) -> BodyTraversalPlan<'document> {
    fn append<'document>(
        document: &'document AstDocument,
        nodes: &[crate::presentation::LayoutNode],
        policy: &RenderPolicy,
        scope: RenderScope,
        steps: &mut Vec<BodyTraversalStep<'document>>,
    ) {
        for node in nodes {
            match node {
                crate::presentation::LayoutNode::Generated(
                    crate::presentation::GeneratedLayoutNode::TableOfContents,
                ) => steps.push(BodyTraversalStep::TableOfContents),
                crate::presentation::LayoutNode::Generated(
                    crate::presentation::GeneratedLayoutNode::FootnoteCatalog,
                ) => steps.push(BodyTraversalStep::FootnoteCatalog),
                crate::presentation::LayoutNode::Section {
                    scope: layout_scope,
                    nodes,
                } => append(document, nodes, policy, scope.enter(*layout_scope), steps),
                crate::presentation::LayoutNode::Block(block_id) => {
                    let block = document
                        .top_level_block(*block_id)
                        .expect("layout only contains top-level blocks");
                    steps.push(BodyTraversalStep::Block {
                        block,
                        scope,
                        render_header_metadata: matches!(
                            block,
                            AstBlock::Heading(Heading {
                                kind: HeadingKind::DocumentTitle,
                                ..
                            })
                        ) && policy.render_document_title,
                    });
                }
            }
        }
    }

    let mut steps = Vec::new();
    append(
        document,
        document.layout().nodes(),
        policy,
        RenderScope::default(),
        &mut steps,
    );
    BodyTraversalPlan { steps }
}

pub(super) struct InlinePlan {
    nodes: Vec<InlineNode>,
}

enum InlineNode {
    Text {
        value: String,
        collapse_line_breaks: bool,
    },
    Element(PlannedElement),
}

struct PlannedElement {
    name: ElementName<'static>,
    attributes: Vec<PlannedAttribute>,
    children: Vec<InlineNode>,
    close: bool,
    line_break_after: bool,
}

pub(super) enum PlannedAttribute {
    Passive(PassiveAttributeName<'static>, String),
    ActiveUrl(ActiveUrlAttributeName<'static>, OwnedSafeUrl),
    FragmentUrl(ActiveUrlAttributeName<'static>, OwnedSafeFragmentUrl),
    Classes(Vec<ClassName<'static>>),
    SourceLanguage(SourceLanguageClass),
    Boolean(BooleanAttributeName<'static>),
}

pub(super) struct BlockWriter;

impl BlockWriter {
    pub(super) fn start(output: &mut String, name: &'static str, attributes: &[PlannedAttribute]) {
        let mut writer = HtmlWriter::new(output);
        writer.start(element(name));
        serialize_attributes(&mut writer, attributes);
        writer.finish_start();
    }

    pub(super) fn end(output: &mut String, name: &'static str) {
        HtmlWriter::new(output).end(element(name));
    }

    pub(super) fn void(output: &mut String, name: &'static str, attributes: &[PlannedAttribute]) {
        Self::start(output, name, attributes);
    }

    pub(super) fn text(output: &mut String, value: &str) {
        HtmlWriter::new(output).text(TextValue::new(value));
    }

    pub(super) fn inline_text(output: &mut String, value: &str) {
        HtmlWriter::new(output).inline_text(TextValue::new(value));
    }

    pub(super) fn line_break(output: &mut String) {
        HtmlWriter::new(output).line_break();
    }
}

pub(super) fn plan_inlines(
    inlines: &[Inline],
    context: &mut InlineRenderContext<'_, '_>,
) -> InlinePlan {
    let mut nodes = Vec::new();
    plan_sequence(inlines, context, &mut nodes);
    InlinePlan { nodes }
}

pub(super) fn serialize_inlines(output: &mut String, plan: &InlinePlan) {
    serialize_nodes(output, &plan.nodes);
}

fn plan_sequence(
    inlines: &[Inline],
    context: &mut InlineRenderContext<'_, '_>,
    output: &mut Vec<InlineNode>,
) {
    for inline in inlines {
        match inline {
            Inline::Text(text) => inline_text(output, &text.value),
            Inline::Literal { kind, value, .. } => match kind {
                InlineLiteralKind::Monospace => output.push(element_with_children(
                    "code",
                    Vec::new(),
                    vec![inline_text_node(value)],
                )),
            },
            Inline::Styled {
                style, children, ..
            } => {
                if matches!(
                    style,
                    InlineStyle::CurvedDoubleQuote | InlineStyle::CurvedSingleQuote
                ) {
                    inline_text(
                        output,
                        if *style == InlineStyle::CurvedDoubleQuote {
                            "“"
                        } else {
                            "‘"
                        },
                    );
                    plan_sequence(children, context, output);
                    inline_text(
                        output,
                        if *style == InlineStyle::CurvedDoubleQuote {
                            "”"
                        } else {
                            "’"
                        },
                    );
                } else {
                    let name = match style {
                        InlineStyle::Strong => "strong",
                        InlineStyle::Emphasis => "em",
                        InlineStyle::Highlight => "mark",
                        InlineStyle::Subscript => "sub",
                        InlineStyle::Superscript => "sup",
                        InlineStyle::CurvedDoubleQuote | InlineStyle::CurvedSingleQuote => {
                            unreachable!("curved quotes are planned as text")
                        }
                    };
                    let mut planned = Vec::new();
                    plan_sequence(children, context, &mut planned);
                    output.push(element_with_children(name, Vec::new(), planned));
                }
            }
            Inline::AttributeReference { name, value, .. } => {
                if let Some(value) = value {
                    plan_attribute_value(value, output);
                } else {
                    inline_text(output, "{");
                    text(output, name);
                    inline_text(output, "}");
                }
            }
            Inline::Link(link) => plan_link(link, context, output),
            Inline::Reference(reference) => plan_reference(reference, context, output),
            Inline::Macro(node) => plan_standard_macro(node, context, output),
            Inline::HardBreak { .. } => output.push(void_element("br", Vec::new(), true)),
            Inline::Passthrough { value, .. } => inline_text(output, value),
            Inline::Formula(formula) => {
                let mut attributes = Vec::new();
                if context
                    .policy
                    .math_languages
                    .allowed
                    .contains(&formula.language)
                {
                    attributes.extend(math_attributes(formula.language, "inline"));
                } else {
                    context.diagnostics.push(render_diagnostic(
                        "math-language-not-allowed",
                        "math language is rejected by the render policy",
                        formula.range,
                    ));
                }
                output.push(element_with_children(
                    "code",
                    attributes,
                    vec![inline_text_node(&formula.value)],
                ));
            }
        }
    }
}

fn plan_attribute_value(value: &str, output: &mut Vec<InlineNode>) {
    let mut remaining = value;
    while let Some(index) = remaining.find(" +\n") {
        text(output, &remaining[..index]);
        output.push(void_element("br", Vec::new(), true));
        remaining = &remaining[index + 3..];
    }
    text(output, remaining);
}

fn plan_link(link: &Link, context: &mut InlineRenderContext<'_, '_>, output: &mut Vec<InlineNode>) {
    match plan::plan_link(link, context.policy) {
        plan::PlannedLink::Active {
            href,
            new_context,
            noreferrer,
        } => {
            let mut attributes = vec![active_url("href", href.into_owned())];
            if new_context {
                attributes.push(passive("target", "_blank"));
                attributes.push(passive(
                    "rel",
                    if noreferrer {
                        "noopener noreferrer"
                    } else {
                        "noopener"
                    },
                ));
            }
            output.push(element_with_children(
                "a",
                attributes,
                plan_label_or_text(&link.label, &link.target_source, context),
            ));
        }
        plan::PlannedLink::Fallback { diagnostic } => {
            output.extend(plan_label_or_text(
                &link.label,
                &link.target_source,
                context,
            ));
            append_plan_diagnostics(context.diagnostics, [diagnostic]);
        }
    }
}

fn plan_reference(
    reference: &Reference,
    context: &mut InlineRenderContext<'_, '_>,
    output: &mut Vec<InlineNode>,
) {
    let planned = plan::plan_reference(
        reference,
        context.identifiers,
        context.policy,
        context.input_usage,
    );
    if let Some(href) = planned.href {
        let href = match href {
            PlannedReferenceHref::Local(anchor) => fragment_url("href", anchor.into_owned()),
            PlannedReferenceHref::Resolved(href) => active_url("href", href.into_owned()),
        };
        let mut attributes = vec![href];
        if context.catalogs.bibliography().iter().any(|entry| {
            entry
                .references
                .iter()
                .any(|candidate| candidate.range == reference.range)
        }) {
            attributes.push(passive("id", bibliography_reference_id(reference.range)));
        }
        output.push(element_with_children(
            "a",
            attributes,
            plan_label_or_text(&reference.label, &planned.fallback, context),
        ));
    } else {
        match context.policy.unresolved_references {
            UnresolvedReferencePresentation::Target => output.extend(plan_label_or_text(
                &reference.label,
                &planned.fallback,
                context,
            )),
            UnresolvedReferencePresentation::LabelOnly => {
                plan_sequence(&reference.label, context, output);
            }
            UnresolvedReferencePresentation::Hidden => {}
        }
    }
    append_plan_diagnostics(context.diagnostics, planned.diagnostics);
}

fn plan_label_or_text(
    label: &[Inline],
    fallback: &str,
    context: &mut InlineRenderContext<'_, '_>,
) -> Vec<InlineNode> {
    let mut nodes = Vec::new();
    if label.is_empty() {
        text(&mut nodes, fallback);
    } else {
        plan_sequence(label, context, &mut nodes);
    }
    nodes
}

fn plan_standard_macro(
    node: &crate::inline::StandardMacro,
    context: &mut InlineRenderContext<'_, '_>,
    output: &mut Vec<InlineNode>,
) {
    use crate::inline::StandardMacroKind as Kind;
    let first = node
        .attributes
        .first()
        .map(|attribute| attribute.value.as_str());
    match node.kind {
        Kind::Email => {
            let href = format!("mailto:{}", node.target);
            let Some(href) = OwnedSafeUrl::from_policy(
                href,
                &context.policy.active_urls,
                UrlProvenance::Authored,
            ) else {
                inline_text(output, &node.target);
                return;
            };
            output.push(element_with_children(
                "a",
                vec![active_url("href", href)],
                vec![inline_text_node(&node.target)],
            ));
        }
        Kind::Footnote => {
            let Some((footnote, occurrence)) = context.catalogs.footnote_occurrence(node.range)
            else {
                inline_text(output, first.unwrap_or(&node.target));
                return;
            };
            let number = footnote.number.to_string();
            let reference_id = format!("_footnoteref_{}_{}", footnote.number, occurrence + 1);
            let target = format!("_footnote_{}", footnote.number);
            let href = SafeFragmentUrl::new(&target)
                .expect("generated footnote targets are nonempty and control-free")
                .into_owned();
            output.push(element_with_children(
                "sup",
                vec![classes(&["footnote"])],
                vec![element_with_children(
                    "a",
                    vec![
                        classes(&["footnote-ref"]),
                        passive("id", reference_id),
                        fragment_url("href", href),
                    ],
                    vec![text_node(&number)],
                )],
            ));
        }
        Kind::Anchor | Kind::BibliographyAnchor => {
            let mut attributes = vec![passive("id", &node.target)];
            if node.kind == Kind::BibliographyAnchor {
                attributes.push(classes(&["bibliography-anchor"]));
            }
            output.push(element_with_children("span", attributes, Vec::new()));
        }
        // A citation has no display text of its own. A key that names an entry
        // defined in this document links to it. Any other key belongs to a
        // library outside the document, so until a host resolves it the same
        // policy that governs an unresolved cross reference decides whether the
        // key stays visible.
        Kind::Citation => {
            // Positional attributes are citation keys in source order. Named
            // attributes such as `locator` describe the citation, not a key.
            let keys = node
                .attributes
                .iter()
                .filter(|key| key.name.is_none())
                .collect::<Vec<_>>();
            if let Some(segments) = plan_resolved_citation(node.range, context) {
                // The entries this document defines already link back to each
                // key, so those landing points must survive even though the
                // host's text replaced the keys themselves.
                let mut children = keys
                    .iter()
                    .filter(|key| {
                        context
                            .catalogs
                            .bibliography()
                            .iter()
                            .any(|entry| entry.id == key.value)
                    })
                    .map(|key| {
                        element_with_children(
                            "span",
                            vec![passive("id", bibliography_reference_id(key.value_range))],
                            Vec::new(),
                        )
                    })
                    .collect::<Vec<_>>();
                children.extend(segments);
                output.push(element_with_children(
                    "span",
                    vec![classes(&["citation"])],
                    children,
                ));
                return;
            }
            for key in keys {
                let href = context
                    .catalogs
                    .bibliography()
                    .iter()
                    .find(|entry| entry.id == key.value)
                    .and_then(|entry| SafeFragmentUrl::new(&entry.id))
                    .map(SafeFragmentUrl::into_owned);
                if let Some(href) = href {
                    output.push(element_with_children(
                        "a",
                        vec![
                            classes(&["citation"]),
                            passive("id", bibliography_reference_id(key.value_range)),
                            fragment_url("href", href),
                        ],
                        vec![inline_text_node(&key.value)],
                    ));
                    continue;
                }
                match context.policy.unresolved_references {
                    UnresolvedReferencePresentation::Target
                    | UnresolvedReferencePresentation::LabelOnly => {
                        output.push(element_with_children(
                            "span",
                            vec![classes(&["citation"])],
                            vec![inline_text_node(&key.value)],
                        ));
                    }
                    UnresolvedReferencePresentation::Hidden => {}
                }
            }
        }
        Kind::IndexTerm => output.push(element_with_children(
            "span",
            vec![classes(&["index-term"])],
            Vec::new(),
        )),
        Kind::Keyboard => {
            if context.policy.render_ui_macros {
                output.push(element_with_children(
                    "kbd",
                    Vec::new(),
                    vec![inline_text_node(first.unwrap_or(&node.target))],
                ));
            } else {
                inline_text(output, first.unwrap_or(&node.target));
            }
        }
        Kind::Button => {
            if context.policy.render_ui_macros {
                output.push(element_with_children(
                    "span",
                    vec![classes(&["button"])],
                    vec![inline_text_node(first.unwrap_or(&node.target))],
                ));
            } else {
                inline_text(output, first.unwrap_or(&node.target));
            }
        }
        Kind::Menu => {
            if !context.policy.render_ui_macros {
                inline_text(output, first.unwrap_or(&node.target));
                return;
            }
            let mut children = vec![inline_text_node(&node.target)];
            for attribute in &node.attributes {
                children.push(inline_text_node(" › "));
                children.push(inline_text_node(&attribute.value));
            }
            output.push(element_with_children(
                "span",
                vec![classes(&["menu"])],
                children,
            ));
        }
        Kind::Image | Kind::Icon => plan_image_macro(node, context, output),
        Kind::Audio | Kind::Video => plan_media_macro(node, context, output),
    }
}

fn plan_image_macro(
    node: &crate::inline::StandardMacro,
    context: &mut InlineRenderContext<'_, '_>,
    output: &mut Vec<InlineNode>,
) {
    let alt = if node.kind == crate::inline::StandardMacroKind::Icon {
        macro_attribute(node, "alt", usize::MAX)
            .or_else(|| macro_attribute(node, "title", usize::MAX))
            .unwrap_or(&node.target)
    } else {
        macro_attribute(node, "alt", 0).unwrap_or("")
    };
    if !context.policy.resources.images {
        inline_text(output, alt);
        context.diagnostics.push(render_diagnostic(
            "resource-capability-disabled",
            "image rendering is disabled by the host capability profile",
            node.range,
        ));
        return;
    }
    let resource = plan::plan_resource(
        node.range,
        node.target_range,
        MediaFamily::Image,
        context.policy,
        context.input_usage,
    );
    append_plan_diagnostics(context.diagnostics, resource.diagnostics);
    let Some(src) = resource.value else {
        inline_text(output, alt);
        return;
    };
    let mut attributes = vec![active_url("src", src.into_owned()), passive("alt", alt)];
    let positional_dimensions = node.kind == crate::inline::StandardMacroKind::Image;
    append_dimension(
        &mut attributes,
        node,
        "width",
        positional_dimensions.then_some(1),
    );
    append_dimension(
        &mut attributes,
        node,
        "height",
        positional_dimensions.then_some(2),
    );
    if let Some(title) = macro_attribute(node, "title", usize::MAX) {
        attributes.push(passive("title", title));
    }
    output.push(void_element("img", attributes, false));
}

fn plan_media_macro(
    node: &crate::inline::StandardMacro,
    context: &mut InlineRenderContext<'_, '_>,
    output: &mut Vec<InlineNode>,
) {
    let title = macro_attribute(node, "title", 0);
    let fallback = title.unwrap_or(&node.target);
    if !context.policy.resources.media {
        inline_text(output, fallback);
        context.diagnostics.push(render_diagnostic(
            "resource-capability-disabled",
            "media rendering is disabled by the host capability profile",
            node.range,
        ));
        return;
    }
    let (name, family) = if node.kind == crate::inline::StandardMacroKind::Audio {
        ("audio", MediaFamily::Audio)
    } else {
        ("video", MediaFamily::Video)
    };
    let resource = plan::plan_resource(
        node.range,
        node.target_range,
        family,
        context.policy,
        context.input_usage,
    );
    append_plan_diagnostics(context.diagnostics, resource.diagnostics);
    let Some(src) = resource.value else {
        inline_text(output, fallback);
        return;
    };
    let mut attributes = vec![active_url("src", src.into_owned()), boolean("controls")];
    if node.kind == crate::inline::StandardMacroKind::Video {
        append_dimension(&mut attributes, node, "width", Some(1));
        append_dimension(&mut attributes, node, "height", Some(2));
        if let Some(poster) =
            macro_attribute_node(node, "poster").filter(|poster| !poster.value.is_empty())
        {
            if !context.policy.resources.images {
                context.diagnostics.push(render_diagnostic(
                    "resource-capability-disabled",
                    "poster rendering is disabled by the host capability profile",
                    poster.value_range,
                ));
            } else {
                let poster = plan::plan_resource(
                    poster.value_range,
                    poster.value_range,
                    MediaFamily::Image,
                    context.policy,
                    context.input_usage,
                );
                append_plan_diagnostics(context.diagnostics, poster.diagnostics);
                if let Some(poster) = poster.value {
                    attributes.push(active_url("poster", poster.into_owned()));
                }
            }
        }
    }
    if let Some(title) = title {
        attributes.push(passive("title", title));
    }
    output.push(element_with_children(name, attributes, Vec::new()));
}

fn macro_attribute_node<'a>(
    node: &'a crate::inline::StandardMacro,
    name: &str,
) -> Option<&'a crate::inline::MacroAttribute> {
    node.attributes
        .iter()
        .find(|attribute| attribute.name.as_deref() == Some(name))
}

fn macro_attribute<'a>(
    node: &'a crate::inline::StandardMacro,
    name: &str,
    position: usize,
) -> Option<&'a str> {
    node.attributes
        .iter()
        .find(|attribute| attribute.name.as_deref() == Some(name))
        .or_else(|| {
            node.attributes
                .get(position)
                .filter(|attribute| attribute.name.is_none())
        })
        .map(|attribute| attribute.value.as_str())
}

fn append_dimension(
    attributes: &mut Vec<PlannedAttribute>,
    node: &crate::inline::StandardMacro,
    name: &'static str,
    position: Option<usize>,
) {
    let value = node
        .attributes
        .iter()
        .find(|attribute| attribute.name.as_deref() == Some(name))
        .or_else(|| {
            position.and_then(|position| {
                node.attributes
                    .get(position)
                    .filter(|attribute| attribute.name.is_none())
            })
        })
        .map(|attribute| attribute.value.as_str());
    if let Some(value) = value
        && !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
    {
        attributes.push(passive(name, value));
    }
}

fn math_attributes(
    language: crate::inline::MathLanguage,
    display: &'static str,
) -> Vec<PlannedAttribute> {
    vec![
        classes(&[match language {
            crate::inline::MathLanguage::Latex => "math-latex",
            crate::inline::MathLanguage::Typst => "math-typst",
        }]),
        passive("data-math-language", language.as_asciidoc_name()),
        passive("data-math-display", display),
    ]
}

fn serialize_nodes(output: &mut String, nodes: &[InlineNode]) {
    for node in nodes {
        match node {
            InlineNode::Text {
                value,
                collapse_line_breaks,
            } => {
                let mut writer = HtmlWriter::new(output);
                if *collapse_line_breaks {
                    writer.inline_text(TextValue::new(value));
                } else {
                    writer.text(TextValue::new(value));
                }
            }
            InlineNode::Element(element) => {
                let mut writer = HtmlWriter::new(output);
                writer.start(element.name);
                serialize_attributes(&mut writer, &element.attributes);
                writer.finish_start();
                serialize_nodes(output, &element.children);
                if element.close {
                    HtmlWriter::new(output).end(element.name);
                }
                if element.line_break_after {
                    output.push('\n');
                }
            }
        }
    }
}

fn serialize_attributes(writer: &mut HtmlWriter<'_>, attributes: &[PlannedAttribute]) {
    for attribute in attributes {
        match attribute {
            PlannedAttribute::Passive(name, value) => {
                writer.passive_attribute(*name, AttributeValue::new(value))
            }
            PlannedAttribute::ActiveUrl(name, value) => {
                writer.owned_active_url_attribute(*name, value);
            }
            PlannedAttribute::FragmentUrl(name, value) => {
                writer.owned_fragment_url_attribute(*name, value);
            }
            PlannedAttribute::Classes(classes) => writer.class_attribute(classes),
            PlannedAttribute::SourceLanguage(class) => {
                writer.source_language_class_attribute(class);
            }
            PlannedAttribute::Boolean(name) => writer.boolean_attribute(*name),
        }
    }
}

fn element_with_children(
    name: &'static str,
    attributes: Vec<PlannedAttribute>,
    children: Vec<InlineNode>,
) -> InlineNode {
    InlineNode::Element(PlannedElement {
        name: element(name),
        attributes,
        children,
        close: true,
        line_break_after: false,
    })
}

fn void_element(
    name: &'static str,
    attributes: Vec<PlannedAttribute>,
    line_break_after: bool,
) -> InlineNode {
    InlineNode::Element(PlannedElement {
        name: element(name),
        attributes,
        children: Vec::new(),
        close: false,
        line_break_after,
    })
}

fn text(output: &mut Vec<InlineNode>, value: &str) {
    output.push(text_node(value));
}

fn inline_text(output: &mut Vec<InlineNode>, value: &str) {
    output.push(inline_text_node(value));
}

fn text_node(value: &str) -> InlineNode {
    InlineNode::Text {
        value: value.to_owned(),
        collapse_line_breaks: false,
    }
}

fn inline_text_node(value: &str) -> InlineNode {
    InlineNode::Text {
        value: value.to_owned(),
        collapse_line_breaks: true,
    }
}

pub(super) fn passive(name: &'static str, value: impl Into<String>) -> PlannedAttribute {
    PlannedAttribute::Passive(passive_name(name), value.into())
}

fn active_url(name: &'static str, value: OwnedSafeUrl) -> PlannedAttribute {
    PlannedAttribute::ActiveUrl(active_name(name), value)
}

pub(super) fn fragment_url(name: &'static str, value: OwnedSafeFragmentUrl) -> PlannedAttribute {
    PlannedAttribute::FragmentUrl(active_name(name), value)
}

pub(super) fn classes(values: &[&'static str]) -> PlannedAttribute {
    PlannedAttribute::Classes(
        values
            .iter()
            .map(|value| ClassName::new(value).expect("inline plan uses allowlisted HTML classes"))
            .collect(),
    )
}

fn boolean(name: &'static str) -> PlannedAttribute {
    PlannedAttribute::Boolean(boolean_name(name))
}

pub(super) fn source_language_class(language: &str) -> PlannedAttribute {
    PlannedAttribute::SourceLanguage(
        SourceLanguageClass::new(language)
            .expect("source language classes require a nonempty normalized language"),
    )
}

fn element(name: &'static str) -> ElementName<'static> {
    ElementName::new(name).expect("inline plan uses allowlisted HTML elements")
}

/// Plans the children of a citation the host resolved, if it resolved this one.
///
/// The host owns the citation text, so it arrives as data and never as markup:
/// every segment is escaped, and an anchor is honoured only when this document
/// really defines that target. A segment naming an unknown anchor is reported
/// and stays as plain text, so a stale bibliography cannot produce a dead link.
/// `None` means the host said nothing about this citation.
fn plan_resolved_citation(
    range: crate::source::TextRange,
    context: &mut InlineRenderContext<'_, '_>,
) -> Option<Vec<InlineNode>> {
    let outcome = match context.input_usage.citation_at(range) {
        crate::render::ResolutionMatch::Unique(resolution) => &resolution.outcome,
        // A duplicate is already reported as a render input problem, and a
        // missing resolution simply leaves the keys to the unresolved policy.
        crate::render::ResolutionMatch::Duplicate | crate::render::ResolutionMatch::Missing => {
            return None;
        }
    };
    let segments = match outcome {
        crate::citation::CitationOutcome::Resolved { segments } => segments,
        crate::citation::CitationOutcome::Failed(failure) => {
            context.diagnostics.push(render_diagnostic(
                failure.kind.diagnostic_code(),
                "the host could not resolve this citation",
                range,
            ));
            return None;
        }
    };
    let mut children = Vec::new();
    for segment in segments {
        let href = segment
            .anchor
            .as_deref()
            .filter(|anchor| context.identifiers.target_by_id(anchor).is_some())
            .and_then(SafeFragmentUrl::new)
            .map(SafeFragmentUrl::into_owned);
        match (href, segment.anchor.as_deref()) {
            (Some(href), _) => children.push(element_with_children(
                "a",
                vec![fragment_url("href", href)],
                vec![inline_text_node(&segment.text)],
            )),
            (None, Some(anchor)) => {
                context.diagnostics.push(render_diagnostic(
                    "unknown-citation-anchor",
                    &format!("resolved citation names the unknown anchor `{anchor}`"),
                    range,
                ));
                children.push(inline_text_node(&segment.text));
            }
            (None, None) => children.push(inline_text_node(&segment.text)),
        }
    }
    Some(children)
}

fn passive_name(name: &'static str) -> PassiveAttributeName<'static> {
    PassiveAttributeName::new(name).expect("inline plan uses allowlisted passive attributes")
}

fn active_name(name: &'static str) -> ActiveUrlAttributeName<'static> {
    ActiveUrlAttributeName::new(name).expect("inline plan uses active URL attributes")
}

fn boolean_name(name: &'static str) -> BooleanAttributeName<'static> {
    BooleanAttributeName::new(name).expect("inline plan uses allowlisted boolean attributes")
}

#[cfg(test)]
mod tests {
    use crate::parser::parse;

    use super::*;

    #[test]
    fn traversal_plan_flattens_generated_nodes_and_preserves_section_scope() {
        let parsed = parse(
            "= References\n:toc:\n\n[bibliography]\n== Sources\n\n* bibanchor:ref[] Entry\n\n== After\n",
        )
        .expect("valid document");
        let plan = plan_body_traversal(&parsed.ast, &RenderPolicy::default());

        assert!(matches!(
            plan.steps.first(),
            Some(BodyTraversalStep::Block {
                render_header_metadata: true,
                ..
            })
        ));
        assert!(
            plan.steps
                .iter()
                .any(|step| matches!(step, BodyTraversalStep::TableOfContents))
        );
        let bibliography_blocks = plan
            .steps
            .iter()
            .filter(|step| {
                matches!(
                    step,
                    BodyTraversalStep::Block {
                        scope: RenderScope {
                            bibliography_section: true
                        },
                        ..
                    }
                )
            })
            .count();
        assert_eq!(bibliography_blocks, 2);
        assert!(matches!(
            plan.steps
                .iter()
                .rev()
                .find(|step| matches!(step, BodyTraversalStep::Block { .. })),
            Some(BodyTraversalStep::Block {
                scope: RenderScope {
                    bibliography_section: false
                },
                ..
            })
        ));
    }

    #[test]
    fn serializer_accepts_only_typed_nodes_and_preserves_attribute_order() {
        let href = OwnedSafeUrl::from_policy(
            "https://example.test/?a=1&b=2".to_owned(),
            &crate::url::ActiveUrlPolicy::default(),
            UrlProvenance::Authored,
        )
        .expect("safe URL");
        let plan = InlinePlan {
            nodes: vec![element_with_children(
                "a",
                vec![
                    classes(&["footnote-ref"]),
                    passive("id", "<id>"),
                    active_url("href", href),
                ],
                vec![inline_text_node("<label>\nnext")],
            )],
        };
        let mut output = String::new();
        serialize_inlines(&mut output, &plan);
        assert_eq!(
            output,
            "<a class=\"footnote-ref\" id=\"&lt;id&gt;\" href=\"https://example.test/?a=1&amp;b=2\">&lt;label&gt; next</a>"
        );
    }

    #[test]
    fn attribute_continuation_is_planned_as_one_line_break() {
        let mut nodes = Vec::new();
        plan_attribute_value("before +\nafter", &mut nodes);
        let mut output = String::new();
        serialize_inlines(&mut output, &InlinePlan { nodes });
        assert_eq!(output, "before<br>\nafter");
    }
}
