use adocweave::AnalysisOptions;

use crate::{CliError, analyze, decode_input};

pub(crate) fn process(
    input: &[u8],
    analysis_options: &AnalysisOptions,
) -> Result<String, CliError> {
    let source = decode_input(input)?;
    let analysis = analyze(source, analysis_options)?;
    Ok(adocweave::semantic::render_symbols_json(
        &adocweave::semantic::document_symbols(analysis.document()),
    ))
}
