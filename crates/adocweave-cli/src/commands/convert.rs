use std::io;
use std::path::Path;

use adocweave::output::html::RenderPolicy;
use adocweave::{AnalysisOptions, Engine, ParseError};

use super::html_policy::{self, StylesheetArgument, StylesheetFileOrigin};

#[derive(Debug)]
pub(crate) enum Error {
    InvalidUtf8 { valid_up_to: usize },
    Analysis(ParseError),
    Html(html_policy::Error),
}

pub(crate) fn run(
    input: &[u8],
    analysis_options: &AnalysisOptions,
    html: &adocweave_config::HtmlSettings,
    complete: bool,
    stylesheets: &[StylesheetArgument],
    mut read: impl FnMut(StylesheetFileOrigin, &Path) -> io::Result<Vec<u8>>,
) -> Result<String, Error> {
    let policy = html_policy::build(html, complete, stylesheets, &mut read, || false)
        .map_err(Error::Html)?;
    process(input, analysis_options, &policy)
}

fn process(
    input: &[u8],
    analysis_options: &AnalysisOptions,
    render_policy: &RenderPolicy,
) -> Result<String, Error> {
    let source = std::str::from_utf8(input).map_err(|error| Error::InvalidUtf8 {
        valid_up_to: error.valid_up_to(),
    })?;
    let analysis = Engine::new(analysis_options.clone())
        .analyze(source)
        .map_err(Error::Analysis)?;
    Ok(
        html_policy::render_checked(analysis.document(), render_policy)
            .map_err(Error::Html)?
            .html,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_owns_policy_input_decoding_analysis_and_rendering() {
        let output = run(
            b"= Title\n\nBody\n",
            &AnalysisOptions::default(),
            &adocweave_config::HtmlSettings::default(),
            false,
            &[],
            |_, _| unreachable!("no stylesheet files"),
        )
        .expect("converted output");

        assert_eq!(
            output,
            "<h1 class=\"document-title\" id=\"_title\">Title</h1>\n<p>Body</p>\n"
        );
    }

    #[test]
    fn run_reports_the_invalid_utf8_offset() {
        let error = run(
            b"valid\xff",
            &AnalysisOptions::default(),
            &adocweave_config::HtmlSettings::default(),
            false,
            &[],
            |_, _| unreachable!("no stylesheet files"),
        )
        .expect_err("invalid UTF-8");

        assert!(matches!(error, Error::InvalidUtf8 { valid_up_to: 5 }));
    }
}
