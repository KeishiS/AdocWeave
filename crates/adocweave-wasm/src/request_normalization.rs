//! Cross-field validation at the JSON request boundary.

use crate::request_wire::WasmRequest;
use crate::{VERSION, WasmError, render_inputs};

/// A request whose package version and cross-field invariants were validated.
///
/// The inner wire value is private so the core conversion stage cannot be
/// called with an unnormalized public request.
pub(crate) struct NormalizedRequest(WasmRequest);

pub(crate) fn normalize(mut request: WasmRequest) -> Result<NormalizedRequest, WasmError> {
    if request.package_version != VERSION {
        return Err(WasmError {
            code: "unsupported-api-version".to_owned(),
            message: format!(
                "unsupported package version {} (expected {VERSION})",
                request.package_version
            ),
        });
    }
    let mut external_attributes = request.analysis_options.attributes.clone();
    if let Some(input) = &mut request.preprocess {
        if external_attributes.is_empty() {
            external_attributes.clone_from(&input.options.attributes);
        } else if !input.options.attributes.is_empty()
            && input.options.attributes != external_attributes
        {
            return Err(WasmError {
                code: "invalid-options".to_owned(),
                message: "analysisOptions.attributes and preprocess.options.attributes must agree"
                    .to_owned(),
            });
        }
        input.options.attributes.clone_from(&external_attributes);
    }
    render_inputs::validate(
        &request.render_inputs,
        &request.analysis_options.syntax.limits,
        &request.output_limits,
    )?;
    request.analysis_options.attributes = external_attributes;
    Ok(NormalizedRequest(request))
}

impl NormalizedRequest {
    pub(super) fn into_wire(self) -> WasmRequest {
        self.0
    }
}
