#![no_main]

use adocweave::html::{RenderPolicy, render};
use adocweave::url::UrlContext;
use adocweave::{Engine, ParseOptions};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|source: &str| {
    if let Ok(analysis) = Engine::new(ParseOptions::default()).analyze(source) {
        let policy = RenderPolicy::default();
        let first = render(&analysis.ast(), &policy);
        let second = render(&analysis.ast(), &policy);
        assert_eq!(first, second);
        for tail in first.html.split("href=\"").skip(1) {
            let href = tail.split('"').next().expect("renderer closes href");
            assert!(
                href.starts_with('#')
                    || policy.allows_url(href, UrlContext::ResolvedReference)
                    || policy.allows_url(href, UrlContext::ResolvedResource)
                    || policy.allows_url(href, UrlContext::AuthoredLink)
            );
        }
    }
});
