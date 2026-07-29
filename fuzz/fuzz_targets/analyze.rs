#![no_main]

use adocweave::text::SyntaxKind;
use adocweave::{AnalysisOptions, Engine, ParseError};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|source: &str| {
    match Engine::new(AnalysisOptions::default()).analyze(source) {
        Ok(analysis) => {
            assert_eq!(analysis.syntax().reconstruct(), source);
            let mut syntax_cursor = 0;
            for node in analysis.syntax().root().descendants() {
                if !matches!(node.kind(), SyntaxKind::Token(_)) {
                    continue;
                }
                let start = node.range().start().to_usize();
                let end = node.range().end().to_usize();
                assert_eq!(start, syntax_cursor);
                assert!(start < end && end <= source.len());
                assert!(source.is_char_boundary(start));
                assert!(source.is_char_boundary(end));
                syntax_cursor = end;
            }
            assert_eq!(syntax_cursor, source.len());
            for token in analysis.syntax().tokens() {
                let range = token.range;
                assert!(range.start() <= range.end());
                assert!(range.end().to_usize() <= source.len());
                assert!(source.is_char_boundary(range.start().to_usize()));
                assert!(source.is_char_boundary(range.end().to_usize()));
            }
        }
        Err(ParseError::InternalInvariant) => {
            panic!("syntax construction invariant failed");
        }
        Err(_) => {}
    }
});
