//! Cross-field validation at the JSON request boundary.

use crate::render_input_normalization::{self, NormalizedRenderInputs};
use crate::request_wire::WasmRequest;
use crate::{VERSION, WasmError};

/// A request whose package version and cross-field invariants were validated.
///
/// The inner wire value is private so the core conversion stage cannot be
/// called with an unnormalized public request.
pub(crate) struct NormalizedRequest {
    wire: WasmRequest,
    render_inputs: NormalizedRenderInputs,
}

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
    if let Some(input) = &request.preprocess {
        for (matches, message) in [
            (
                input.options.attributes == request.analysis_options.attributes,
                "analysisOptions.attributes and preprocess.options.attributes must agree",
            ),
            (
                input.options.max_attribute_expansion_depth
                    == request
                        .analysis_options
                        .syntax
                        .limits
                        .max_attribute_expansion_depth,
                "analysis and preprocessing attribute expansion depth limits must agree",
            ),
            (
                input.options.max_attribute_expansion_bytes
                    == request
                        .analysis_options
                        .syntax
                        .limits
                        .max_attribute_expansion_bytes,
                "analysis and preprocessing attribute expansion byte limits must agree",
            ),
        ] {
            if !matches {
                return Err(WasmError {
                    code: "invalid-options".to_owned(),
                    message: message.to_owned(),
                });
            }
        }
    }
    let render_inputs = render_input_normalization::normalize(
        std::mem::take(&mut request.render_inputs),
        &request.analysis_options.syntax.limits,
        &request.output_limits,
    )?;
    Ok(NormalizedRequest {
        wire: request,
        render_inputs,
    })
}

impl NormalizedRequest {
    pub(super) fn into_parts(self) -> (WasmRequest, NormalizedRenderInputs) {
        (self.wire, self.render_inputs)
    }
}
