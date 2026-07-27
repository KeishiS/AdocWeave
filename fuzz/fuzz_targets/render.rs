#![no_main]

use adocweave::output::html::{RenderPolicy, render};
use adocweave::resolution::UrlProvenance;
use adocweave::{AnalysisOptions, Engine};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|source: &str| {
    if let Ok(analysis) = Engine::new(AnalysisOptions::default()).analyze(source) {
        let policy = RenderPolicy::default();
        let first = render(analysis.document(), &policy);
        let second = render(analysis.document(), &policy);
        assert_eq!(first, second);
        for tail in first.html.split("href=\"").skip(1) {
            let href = tail.split('"').next().expect("renderer closes href");
            assert!(
                href.starts_with('#')
                    || policy.allows_url(href, UrlProvenance::ResolvedReference)
                    || policy.allows_url(href, UrlProvenance::ResolvedResource)
                    || policy.allows_url(href, UrlProvenance::Authored)
            );
        }
    }
});
