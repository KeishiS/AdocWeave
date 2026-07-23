#![no_main]

use adocweave::{Engine, ParseOptions};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: &[u8]| {
    let Ok(source) = std::str::from_utf8(input) else {
        return;
    };
    if let Ok(analysis) = Engine::new(ParseOptions::default()).analyze(source) {
        let _ = adocweave::output::html::render(
            analysis.ast(),
            &adocweave::output::html::RenderPolicy::default(),
        );
        let _ = adocweave::output::formatter::format_analysis(
            &analysis,
            &adocweave::output::formatter::FormatConfig::default(),
        );
        let _ = adocweave::semantic::document_symbols(analysis.ast());
        let _ = adocweave::output::diagnostics::render_json(analysis.diagnostics());
    }
});
