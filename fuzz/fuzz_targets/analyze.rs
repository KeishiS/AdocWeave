#![no_main]

use adocweave::{Engine, ParseOptions};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|source: &str| {
    if let Ok(analysis) = Engine::new(ParseOptions::default()).analyze(source) {
        assert_eq!(analysis.syntax().reconstruct(), source);
        for token in analysis.syntax().tokens() {
            let range = token.range;
            assert!(range.start() <= range.end());
            assert!(range.end().to_usize() <= source.len());
            assert!(source.is_char_boundary(range.start().to_usize()));
            assert!(source.is_char_boundary(range.end().to_usize()));
        }
    }
});
