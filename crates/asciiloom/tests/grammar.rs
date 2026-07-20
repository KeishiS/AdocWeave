use asciiloom::inline::{Inline, InlineLiteralKind, InlineStyle};
use asciiloom::parser::{AstBlock, BlockProblemKind, parse};

const SOURCE: &str = include_str!("../../../fixtures/grammar/ambiguous.adoc");

#[test]
fn grammar_ambiguous_fixture_has_normative_ast_and_recovery() {
    let parsed = parse(SOURCE).expect("fixture parses");
    assert_eq!(parsed.cst.reconstruct(), SOURCE);

    let AstBlock::Paragraph(first) = &parsed.ast.blocks[1] else {
        panic!("first content block is a paragraph");
    };
    assert!(
        first.lines[0]
            .inlines
            .iter()
            .all(|inline| matches!(inline, Inline::Text(_)))
    );
    assert!(matches!(
        first.lines[1].inlines.as_slice(),
        [Inline::Styled {
            style: InlineStyle::Strong,
            children,
            ..
        }] if children.iter().any(|child| matches!(
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
    ));
    assert!(!first.lines[2].inline_problems.is_empty());
    assert!(first.lines[2].inlines.iter().any(|inline| matches!(
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

    let diagnostics = asciiloom::lint::lint(SOURCE, &asciiloom::lint::LintConfig::default())
        .expect("fixture lints");
    let codes = diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.as_str())
        .collect::<Vec<_>>();
    assert_eq!(codes, ["unclosed-inline", "unclosed-block"]);
}

#[test]
fn substitutions_keep_opaque_contexts_unparsed_and_html_safe() {
    let parsed = parse(SOURCE).expect("fixture parses");
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

    let html = asciiloom::html::render(&parsed.ast, &asciiloom::html::HtmlOptions::default()).html;
    assert!(html.contains("<pre>*literal* &lt;tag&gt;\n.....\n</pre>"));
    assert!(
        html.contains("<pre><code class=\"language-rust\">_source_ &lt;tag&gt;\n</code></pre>")
    );
    assert!(!html.contains("<em>source</em>"));
    assert!(!html.contains("<tag>"));
}

#[test]
fn grammar_rejects_invalid_source_language_syntax() {
    let parsed = parse("[source, rust]extra]\n----\ncode\n----\n").expect("recoverable source");

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
