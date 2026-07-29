use adocweave::{AnalysisOptions, Engine, ParseError};

#[derive(Debug)]
pub(crate) enum Error {
    InvalidUtf8 { valid_up_to: usize },
    Analysis(ParseError),
}

pub(crate) fn process(input: &[u8], analysis_options: &AnalysisOptions) -> Result<String, Error> {
    let source = std::str::from_utf8(input).map_err(|error| Error::InvalidUtf8 {
        valid_up_to: error.valid_up_to(),
    })?;
    let analysis = Engine::new(analysis_options.clone())
        .analyze(source)
        .map_err(Error::Analysis)?;
    Ok(adocweave::semantic::render_symbols_json(
        &adocweave::semantic::document_symbols(analysis.document()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_owns_input_decoding_and_analysis() {
        let output = process(b"= Title\n\n== Section\n", &AnalysisOptions::default())
            .expect("symbols output");

        assert!(output.contains("\"name\":\"Title\""));
        assert!(output.contains("\"name\":\"Section\""));
    }

    #[test]
    fn process_reports_the_invalid_utf8_offset() {
        let error = process(b"valid\xff", &AnalysisOptions::default()).expect_err("invalid UTF-8");

        assert!(matches!(error, Error::InvalidUtf8 { valid_up_to: 5 }));
    }
}
