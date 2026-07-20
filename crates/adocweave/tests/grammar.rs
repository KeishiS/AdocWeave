use adocweave::inline::{Inline, InlineLiteralKind, InlineStyle};
use adocweave::parser::{AstBlock, BlockProblemKind};
use adocweave::syntax::SyntaxKind;
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
    assert_eq!(parsed.syntax.reconstruct(), SOURCE);
    assert_eq!(
        parsed.syntax.snapshot(),
        include_str!("../../../fixtures/grammar/ambiguous.syntax")
    );
    assert_eq!(
        parsed.ast.snapshot(),
        include_str!("../../../fixtures/grammar/ambiguous.ast")
    );

    let AstBlock::Paragraph(first) = &parsed.ast.blocks[1] else {
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
    assert!(!first.inline_problems.is_empty());
    assert!(first.inlines.iter().any(|inline| matches!(
        inline,
        Inline::Literal {
            kind: InlineLiteralKind::Monospace,
            ..
        }
    )));

    let literals = parsed
        .ast
        .blocks
        .iter()
        .filter_map(|block| match block {
            AstBlock::Literal(literal) => Some(literal),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(literals[0].value, "*literal* <tag>\n.....\n");
    assert!(
        literals[1]
            .problems
            .iter()
            .any(|problem| problem.kind == BlockProblemKind::UnclosedBlock)
    );
    assert!(matches!(
        parsed.ast.blocks.last(),
        Some(AstBlock::Heading(_))
    ));

    let diagnostics =
        adocweave::lint::lint_analysis(&parsed, &adocweave::lint::LintConfig::default())
            .expect("fixture lints");
    assert_eq!(
        adocweave::diagnostic::render_json(&diagnostics),
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
        .ast
        .blocks
        .iter()
        .find_map(|block| match block {
            AstBlock::Source(source) => Some(source),
            _ => None,
        })
        .expect("source block");
    assert_eq!(source_block.value, "_source_ <tag>\n");

    let html = adocweave::html::render(&parsed.ast, &adocweave::html::RenderPolicy::default()).html;
    assert!(html.contains("<pre>*literal* &lt;tag&gt;\n.....\n</pre>"));
    assert!(
        html.contains("<pre><code class=\"language-rust\">_source_ &lt;tag&gt;\n</code></pre>")
    );
    assert!(!html.contains("<em>source</em>"));
    assert!(!html.contains("<tag>"));
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
    assert!(matches!(parsed.ast.blocks[0], AstBlock::Heading(_)));
    assert!(matches!(parsed.ast.blocks[1], AstBlock::Paragraph(_)));
    assert!(matches!(parsed.ast.blocks[2], AstBlock::Unsupported(_)));
    assert!(matches!(parsed.ast.blocks[3], AstBlock::Literal(_)));
    assert!(matches!(parsed.ast.blocks[4], AstBlock::Source(_)));
    assert!(matches!(parsed.ast.blocks[5], AstBlock::Paragraph(_)));

    let html = adocweave::html::render(&parsed.ast, &adocweave::html::RenderPolicy::default()).html;
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
            .ast
            .blocks
            .iter()
            .all(|block| !matches!(block, AstBlock::Source(_)))
    );
    assert!(matches!(
        parsed.ast.blocks.first(),
        Some(AstBlock::Unsupported(_))
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
            .ast
            .blocks
            .iter()
            .all(|block| !matches!(block, AstBlock::Source(_)))
    );
    assert_eq!(
        parsed
            .syntax
            .blocks()
            .iter()
            .map(|block| block.kind())
            .collect::<Vec<_>>(),
        [
            SyntaxKind::Unsupported,
            SyntaxKind::BlankLine,
            SyntaxKind::Unsupported,
            SyntaxKind::Paragraph,
            SyntaxKind::Unsupported,
            SyntaxKind::BlankLine,
            SyntaxKind::Unsupported,
            SyntaxKind::Unsupported,
            SyntaxKind::BlankLine,
        ]
    );
    assert_eq!(parsed.syntax.reconstruct(), source);
}
