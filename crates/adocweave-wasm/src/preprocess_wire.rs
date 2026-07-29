use serde::Serialize;

pub use crate::preprocess_wire_generated::{
    WasmAnalysisPreprocessInput, WasmPreprocessOptions, WasmPreprocessRequest, WasmResource,
    WasmSafeMode,
};

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WasmPreprocessResponse {
    pub package_version: &'static str,
    pub source: String,
    pub source_map: Vec<WasmSourceMapSegment>,
}

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WasmSourceMapSegment {
    pub output_start: u32,
    pub output_end: u32,
    pub source_id: Option<String>,
    pub source_start: u32,
    pub source_end: u32,
    pub mapping: String,
}
