use adocweave::text::{
    LineEnding, LosslessToken, LosslessTokenKind, SourceDocument, SourceLine, SyntaxKind,
};

#[test]
fn public_text_facade_names_every_type_returned_by_source_and_syntax_queries() {
    let source = SourceDocument::new("text\n").expect("valid source");
    let lines: &[SourceLine] = source.lines();
    let tokens: &[LosslessToken] = source.tokens();

    assert_eq!(lines[0].ending(), LineEnding::Lf);
    assert!(matches!(
        tokens.last().map(|token| token.kind),
        Some(LosslessTokenKind::LineEnding(LineEnding::Lf))
    ));
    assert_eq!(
        SyntaxKind::Token(LosslessTokenKind::Text),
        SyntaxKind::Token(tokens[0].kind)
    );
}
