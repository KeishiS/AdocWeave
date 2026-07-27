use adocweave::semantic::{Block, DocumentAttributeOperation, DocumentType, Inline, VerbatimKind};
use adocweave::text::TextSize;
use adocweave::{Analysis, AnalysisOptions, Engine};

fn analyze(source: &str) -> Analysis {
    Engine::new(AnalysisOptions::default())
        .analyze(source)
        .expect("analysis")
}

fn offset(source: &str, needle: &str) -> TextSize {
    TextSize::new(source.find(needle).expect("fixture marker")).expect("fixture offset")
}

#[test]
fn set_redefine_and_unset_are_selected_by_position() {
    let source = include_str!("../../../fixtures/attributes/environment-set-redefine-unset.adoc");
    let analysis = analyze(source);
    let environment = analysis.attribute_environment();

    let alice = environment
        .resolve_at("name", offset(source, "最初は"))
        .expect("first binding");
    assert_eq!(alice.value, Ok(Some("Alice")));
    assert_eq!(alice.binding.id().get(), 0);
    assert_eq!(alice.binding.event_id().get(), 0);
    assert_eq!(alice.binding.operation(), DocumentAttributeOperation::Set);
    assert_eq!(alice.binding.source_text(), "Alice");
    assert_eq!(
        alice.binding.evaluation_at(),
        alice.binding.occurrence().value.source_range.start()
    );
    assert_eq!(
        alice.binding.visible_at(),
        alice.binding.occurrence().range.end()
    );
    let before_visible = TextSize::new(alice.binding.visible_at().to_usize() - 1).expect("before");
    assert!(environment.resolve_at("name", before_visible).is_none());
    assert!(
        environment
            .resolve_at_event("name", alice.binding.visible_position())
            .is_none()
    );
    assert_eq!(
        environment
            .resolve_at("name", alice.binding.visible_at())
            .expect("visible at half-open end")
            .binding
            .id(),
        alice.binding.id()
    );

    let bob = environment
        .resolve_at("name", offset(source, "次は"))
        .expect("replacement binding");
    assert_eq!(bob.value, Ok(Some("Bob")));
    assert_ne!(bob.binding.id(), alice.binding.id());

    let unset = environment
        .resolve_at("name", offset(source, "最後は"))
        .expect("unset binding");
    assert_eq!(unset.value, Ok(None));
    assert_eq!(unset.binding.operation(), DocumentAttributeOperation::Unset);
    assert!(!environment.final_values().contains_key("name"));
    assert_eq!(
        environment
            .history("NAME")
            .map(|binding| binding.id().get())
            .collect::<Vec<_>>(),
        [0, 1, 2]
    );
}

#[test]
fn forward_references_are_not_rebound_and_definition_values_are_snapshots() {
    let forward = include_str!("../../../fixtures/attributes/environment-forward-definition.adoc");
    let analysis = analyze(forward);
    let environment = analysis.attribute_environment();
    let before = offset(forward, "定義前");
    let after = offset(forward, "定義後");

    assert_eq!(
        environment.expand_at("{later}", before),
        Err(adocweave::semantic::AttributeExpansionError::Undefined)
    );
    assert_eq!(
        environment.expand_at("{snapshot}", after),
        Err(adocweave::semantic::AttributeExpansionError::Undefined)
    );
    assert_eq!(
        environment.expand_at("{later}", after),
        Ok("到着".to_owned())
    );

    let definition_time =
        include_str!("../../../fixtures/attributes/environment-definition-time.adoc");
    let analysis = analyze(definition_time);
    let environment = analysis.attribute_environment();
    let content = offset(definition_time, "現在値");
    assert_eq!(
        environment.expand_at("{base}", content),
        Ok("新しい値".to_owned())
    );
    assert_eq!(
        environment.expand_at("{snapshot}", content),
        Ok("古い値".to_owned())
    );
    assert_eq!(
        environment.expand_at("{camelcase}", content),
        Ok("正規化".to_owned())
    );
    assert_eq!(
        environment.expand_at("{CAMELCASE}", content),
        Ok("正規化".to_owned())
    );
}

#[test]
fn self_cycles_fail_at_the_definition_point() {
    let source = include_str!("../../../fixtures/attributes/environment-cycle.adoc");
    let analysis = analyze(source);
    let resolved = analysis
        .attribute_environment()
        .resolve_at("self", offset(source, "値は"))
        .expect("self binding");
    assert_eq!(
        resolved.value,
        Err(adocweave::semantic::AttributeExpansionError::Cycle)
    );
    assert_eq!(
        analysis
            .attribute_environment()
            .resolve_at("propagated", offset(source, "値は"))
            .expect("propagated binding")
            .value,
        Err(adocweave::semantic::AttributeExpansionError::Cycle)
    );
}

#[test]
fn unicode_and_crlf_offsets_select_the_same_environment() {
    let unicode = include_str!("../../../fixtures/attributes/body-unicode.adoc");
    let analysis = analyze(unicode);
    assert_eq!(
        analysis
            .attribute_environment()
            .expand_at("{greeting}", offset(unicode, "{greeting}")),
        Ok("こんにちは🙂".to_owned())
    );

    let crlf = include_str!("../../../fixtures/attributes/body-crlf.adoc");
    let analysis = analyze(crlf);
    assert_eq!(
        analysis
            .attribute_environment()
            .expand_at("{line-ending}", offset(crlf, "後の段落")),
        Ok("crlf".to_owned())
    );
}

#[test]
fn multiline_definitions_use_folded_values_at_definition_time() {
    let source = r#":base: old
:soft: first {base} \
  second
:hard: one + \
  two
:base: new

{soft}
{hard}
"#;
    let analysis = analyze(source);
    let content = offset(source, "{soft}");
    let environment = analysis.attribute_environment();
    assert_eq!(
        environment.expand_at("{soft}", content),
        Ok("first old second".to_owned())
    );
    assert_eq!(
        environment.expand_at("{hard}", content),
        Ok("one +\ntwo".to_owned())
    );
    assert_eq!(
        environment
            .resolve_at("soft", content)
            .expect("soft binding")
            .binding
            .folded_value(),
        "first {base} second"
    );
}

#[test]
fn definition_chains_enforce_depth_and_size_limits() {
    let source = include_str!("../../../fixtures/attributes/environment-limits.adoc");
    let mut options = AnalysisOptions::default();
    options.syntax.limits.max_attribute_expansion_depth = 1;
    options.syntax.limits.max_attribute_expansion_bytes = 8;
    let analysis = Engine::new(options).analyze(source).expect("analysis");
    let environment = analysis.attribute_environment();
    let content = offset(source, "深さは");

    assert_eq!(
        environment
            .resolve_at("level-two", content)
            .expect("depth binding")
            .value,
        Err(adocweave::semantic::AttributeExpansionError::DepthLimitExceeded)
    );
    assert_eq!(
        environment
            .resolve_at("large", content)
            .expect("size binding")
            .value,
        Err(adocweave::semantic::AttributeExpansionError::SizeLimitExceeded)
    );
}

#[test]
fn invalid_occurrences_are_preserved_without_becoming_bindings() {
    let source = include_str!("../../../fixtures/attributes/body-invalid.adoc");
    let analysis = analyze(source);

    assert!(
        analysis
            .document_attribute_occurrences()
            .iter()
            .any(|occurrence| !occurrence.valid)
    );
    assert!(
        analysis
            .attribute_environment()
            .resolve_at("bad name", TextSize::new(source.len()).expect("end"))
            .is_none()
    );
    assert!(
        analysis
            .attribute_environment()
            .resolve_at("valid", TextSize::new(source.len()).expect("end"))
            .is_none()
    );
}

#[test]
fn semantic_consumers_use_their_own_source_positions() {
    let source = include_str!("../../../fixtures/attributes/environment-consumers.adoc");
    let analysis = analyze(source);

    assert_eq!(analysis.links()[0].target, "https://example.test/first");
    assert_eq!(analysis.links()[1].target, "https://example.test/second");
    assert_eq!(analysis.references()[0].expanded_target, "first");
    assert_eq!(analysis.references()[1].expanded_target, "second");
    assert_eq!(analysis.macros()[0].target, "first.png");
    assert_eq!(analysis.macros()[1].target, "second.png");
    let html = adocweave::output::html::render(
        analysis.document(),
        &adocweave::output::html::RenderPolicy::default(),
    );
    assert_eq!(
        html.document_attributes.get("target").map(String::as_str),
        Some("first")
    );
    assert!(html.html.contains(">1. 番号あり</h1>"));
    assert!(html.html.contains(">番号なし</h1>"));
    assert!(html.html.contains(">1. 番号あり</a>"));
    assert!(html.html.contains(">番号なし</a>"));

    let source_block = analysis
        .document()
        .blocks()
        .iter()
        .find_map(|block| match block {
            Block::Verbatim(block) => Some(block),
            _ => None,
        })
        .expect("implicit source block");
    assert!(matches!(
        &source_block.kind,
        VerbatimKind::Source(info) if info.language.as_deref() == Some("rust")
    ));

    let headings = analysis.structure().headings();
    let numbered = analysis
        .presentation()
        .heading_at(headings[1].range)
        .expect("numbered heading");
    let unnumbered = analysis
        .presentation()
        .heading_at(headings[2].range)
        .expect("unnumbered heading");
    assert!(numbered.numbered);
    assert!(!unnumbered.numbered);

    // `doctype` is header-only in the supported profile.
    assert_eq!(analysis.document().header().doctype, DocumentType::Article);
}

#[test]
fn inline_references_follow_set_redefine_and_unset_positions() {
    let source = include_str!("../../../fixtures/attributes/environment-set-redefine-unset.adoc");
    let analysis = analyze(source);
    let values = analysis
        .document()
        .blocks()
        .iter()
        .filter_map(|block| match block {
            Block::Paragraph(paragraph) => Some(&paragraph.inlines),
            _ => None,
        })
        .flatten()
        .filter_map(|inline| match inline {
            Inline::AttributeReference {
                name,
                value,
                expansion_error,
                ..
            } if name == "name" => Some((value.as_deref(), *expansion_error)),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(
        values,
        [
            (Some("Alice"), None),
            (Some("Bob"), None),
            (
                None,
                Some(adocweave::semantic::AttributeExpansionError::Undefined)
            ),
        ]
    );
    let html = adocweave::output::html::render(
        analysis.document(),
        &adocweave::output::html::RenderPolicy::default(),
    )
    .html;
    assert!(html.contains("最初はAliceです。"));
    assert!(html.contains("次はBobです。"));
    assert!(html.contains("最後は{name}です。"));
}

#[test]
fn failed_redefinition_shadows_the_old_value_until_a_successful_binding() {
    let source = include_str!("../../../fixtures/attributes/environment-failed-redefine.adoc");
    let analysis = analyze(source);
    let environment = analysis.attribute_environment();

    assert_eq!(
        environment.expand_at("{value}", offset(source, "最初は")),
        Ok("old".to_owned())
    );
    assert_eq!(
        environment.expand_at("{value}", offset(source, "失敗後は")),
        Err(adocweave::semantic::AttributeExpansionError::Undefined)
    );
    assert_eq!(
        environment.expand_at("{value}", offset(source, "回復後は")),
        Ok("recovered".to_owned())
    );
    assert_eq!(
        environment.final_values().get("value").map(String::as_str),
        Some("recovered")
    );

    let values = analysis
        .document()
        .blocks()
        .iter()
        .filter_map(|block| match block {
            Block::Paragraph(paragraph) => Some(&paragraph.inlines),
            _ => None,
        })
        .flatten()
        .filter_map(|inline| match inline {
            Inline::AttributeReference {
                name,
                value,
                expansion_error,
                ..
            } if name == "value" => Some((value.as_deref(), *expansion_error)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        values,
        [
            (Some("old"), None),
            (
                None,
                Some(adocweave::semantic::AttributeExpansionError::Undefined)
            ),
            (Some("recovered"), None),
        ]
    );
}
