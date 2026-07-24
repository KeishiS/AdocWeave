use adocweave::semantic::{Block, VerbatimKind};
use adocweave::semantic::{Inline, InlineLiteralKind, InlineStyle};
use adocweave::text::{SyntaxIssueClass, SyntaxKind};
use adocweave::{Analysis, Engine, ParseOptions};

const SOURCE: &str = include_str!("../../../fixtures/grammar/ambiguous.adoc");

fn parse(source: &str) -> Analysis {
    Engine::new(ParseOptions::default())
        .analyze(source)
        .expect("fixture analyzes")
}

#[test]
fn grammar_ambiguous_fixture_has_normative_ast_and_recovery() {
    let parsed = parse(SOURCE);
    assert_eq!(parsed.syntax().reconstruct(), SOURCE);
    assert_eq!(
        parsed.syntax().snapshot(),
        include_str!("../../../fixtures/grammar/ambiguous.syntax")
    );
    assert_eq!(
        parsed.document().snapshot(),
        include_str!("../../../fixtures/grammar/ambiguous.ast")
    );

    let Block::Paragraph(first) = &parsed.document().blocks()[1] else {
        panic!("first content block is a paragraph");
    };
    assert!(first.inlines.iter().any(|inline| matches!(
        inline,
        Inline::Styled {
            style: InlineStyle::Strong,
            children,
            ..
        } if children.iter().any(|child| matches!(
            child,
            Inline::Styled {
                style: InlineStyle::Emphasis,
                ..
            }
        )) && children.iter().any(|child| matches!(
            child,
            Inline::Literal {
                kind: InlineLiteralKind::Monospace,
                ..
            }
        ))
    )));
    assert!(
        parsed
            .syntax()
            .issues()
            .iter()
            .any(|issue| issue.class == SyntaxIssueClass::UnclosedInline)
    );
    assert!(first.inlines.iter().any(|inline| matches!(
        inline,
        Inline::Literal {
            kind: InlineLiteralKind::Monospace,
            ..
        }
    )));

    let literals = parsed
        .document()
        .blocks()
        .iter()
        .filter_map(|block| match block {
            Block::Verbatim(block) if matches!(block.kind, VerbatimKind::Literal) => Some(block),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(literals[0].value, "*literal* <tag>\n.....\n");
    assert_eq!(parsed.syntax().nodes(SyntaxKind::Error).count(), 1);
    assert!(matches!(
        parsed.document().blocks().last(),
        Some(Block::Heading(_))
    ));

    let diagnostics = adocweave::output::diagnostics::lint_analysis(
        &parsed,
        &adocweave::output::diagnostics::LintConfig::default(),
    )
    .expect("fixture lints");
    assert_eq!(
        adocweave::output::diagnostics::render_json(&diagnostics),
        include_str!("../../../fixtures/grammar/ambiguous.diagnostics.json").trim_end()
    );
    let codes = diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.as_str())
        .collect::<Vec<_>>();
    assert_eq!(codes, ["unclosed-inline", "unclosed-block"]);
}

#[test]
fn substitutions_keep_opaque_contexts_unparsed_and_html_safe() {
    let parsed = parse(SOURCE);
    let source_block = parsed
        .document()
        .blocks()
        .iter()
        .find_map(|block| match block {
            Block::Verbatim(source) if matches!(source.kind, VerbatimKind::Source(_)) => {
                Some(source)
            }
            _ => None,
        })
        .expect("source block");
    assert_eq!(source_block.value, "_source_ <tag>\n");

    let html = adocweave::output::html::render(
        parsed.document(),
        &adocweave::output::html::RenderPolicy::default(),
    )
    .html;
    assert!(html.contains("<pre>*literal* &lt;tag&gt;\n.....\n</pre>"));
    assert!(
        html.contains("<pre><code class=\"language-rust\">_source_ &lt;tag&gt;\n</code></pre>")
    );
    assert!(!html.contains("<em>source</em>"));
    assert!(!html.contains("<tag>"));
}

#[test]
fn substitution_pipeline_fixture_is_lossless_and_backend_safe() {
    let source = include_str!("../../../fixtures/substitutions/pipeline.adoc");
    let parsed = parse(source);
    assert_eq!(parsed.syntax().reconstruct(), source);
    let html = adocweave::output::html::render(
        parsed.document(),
        &adocweave::output::html::RenderPolicy::default(),
    )
    .html;
    assert!(html.contains("https://example.test"));
    assert!(html.contains("<mark>highlight</mark>"));
    assert!(html.contains("H<sub>2</sub>O E=mc<sup>2</sup>"));
    assert!(html.contains("© “引用”"));
    assert!(html.contains("&lt;script&gt;"));
    assert!(html.contains("&lt;b&gt;置換しない&lt;/b&gt;"));
    assert!(!html.contains("<script>"));
    assert!(!html.contains("<b>置換しない</b>"));
}

#[test]
fn substitutions_cover_every_supported_semantic_context() {
    let source = concat!(
        "= <Title> *strong _nested_ and `code <&>`*\n",
        "\n",
        "Paragraph <tag> & \"double\" 'single' and `code <&>` plus \\*strong*\n",
        "\n",
        "[role=<unsafe>]\n",
        "\n",
        "....\n",
        "*literal* <&>\n",
        "....\n",
        "\n",
        "[source, Rust+Script]\n",
        "----\n",
        "_source_ <&>\n",
        "----\n",
        "\n",
        "https://example.test[label] stem:[x < y]\n",
    );
    let parsed = parse(source);
    assert!(matches!(parsed.document().blocks()[0], Block::Heading(_)));
    assert!(matches!(parsed.document().blocks()[1], Block::Paragraph(_)));
    assert!(matches!(parsed.document().blocks()[2], Block::Unsupported(_)));
    assert!(matches!(
        parsed.document().blocks()[3],
        Block::Verbatim(ref block) if matches!(block.kind, VerbatimKind::Literal)
    ));
    assert!(
        matches!(parsed.document().blocks()[4], Block::Verbatim(ref block) if matches!(block.kind, VerbatimKind::Source(_)))
    );
    assert!(matches!(parsed.document().blocks()[5], Block::Paragraph(_)));

    let html = adocweave::output::html::render(
        parsed.document(),
        &adocweave::output::html::RenderPolicy::default(),
    )
    .html;
    assert!(html.contains(
        "&lt;Title&gt; <strong>strong <em>nested</em> and <code>code &lt;&amp;&gt;</code></strong>"
    ));
    assert!(html.contains(
        "<p>Paragraph &lt;tag&gt; &amp; &#34;double&#34; &#39;single&#39; and \
         <code>code &lt;&amp;&gt;</code> plus *strong*</p>"
    ));
    assert!(html.contains("<p>[role=&lt;unsafe&gt;]</p>"));
    assert!(html.contains("<pre>*literal* &lt;&amp;&gt;\n</pre>"));
    assert!(html.contains(
        "<pre><code class=\"language-rust-script\">_source_ &lt;&amp;&gt;\n</code></pre>"
    ));
    assert!(html.contains(
        "<p><a href=\"https://example.test\">label</a> \
         <code class=\"math-latex\">x &lt; y</code></p>"
    ));
    assert!(!html.contains("<tag>"));
    assert!(!html.contains("<unsafe>"));
}

#[test]
fn grammar_rejects_invalid_source_language_syntax() {
    let parsed = parse("[source, rust]extra]\n----\ncode\n----\n");

    assert!(
        parsed
            .document()
            .blocks()
            .iter()
            .all(|block| !matches!(block, Block::Source(_)))
    );
    assert!(matches!(
        parsed.document().blocks().first(),
        Some(Block::Unsupported(_))
    ));
}

#[test]
fn grammar_source_attribute_requires_an_adjacent_column_zero_delimiter() {
    let source = concat!(
        "[source, rust]\n",
        "\n",
        "----\n",
        "not a source block\n",
        "----\n",
        "\n",
        " [source, rust]\n",
        " ----\n",
    );
    let parsed = parse(source);

    assert!(
        parsed
            .document()
            .blocks()
            .iter()
            .all(|block| !matches!(block, Block::Source(_)))
    );
    assert_eq!(
        parsed
            .syntax()
            .blocks()
            .iter()
            .map(|block| block.kind())
            .collect::<Vec<_>>(),
        [
            SyntaxKind::Unsupported,
            SyntaxKind::BlankLine,
            SyntaxKind::DelimitedBlock,
            SyntaxKind::BlankLine,
            SyntaxKind::LiteralBlock,
            SyntaxKind::BlankLine,
        ]
    );
    assert_eq!(parsed.syntax().reconstruct(), source);
}
