use std::collections::{BTreeMap, BTreeSet};

use adocweave::preprocess::PreprocessOptions;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmPreprocessRequest {
    pub package_version: String,
    pub source_id: Option<String>,
    pub source: String,
    #[serde(default)]
    pub resources: BTreeMap<String, WasmResource>,
    #[serde(default)]
    pub options: WasmPreprocessOptions,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmResource {
    pub source_id: String,
    pub source: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmAnalysisPreprocessInput {
    pub resources: BTreeMap<String, WasmResource>,
    pub options: WasmPreprocessOptions,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct WasmPreprocessOptions {
    pub base_uri: Option<String>,
    pub safe_mode: WasmSafeMode,
    pub allowed_schemes: BTreeSet<String>,
    pub attributes: BTreeMap<String, Option<String>>,
    pub enable_includes: bool,
    pub max_include_depth: u32,
    pub max_includes: u32,
    pub max_total_bytes: u32,
    pub max_expanded_nodes: u32,
    pub max_source_map_segments: u32,
}

impl Default for WasmPreprocessOptions {
    fn default() -> Self {
        let options = PreprocessOptions::default();
        Self {
            base_uri: options.base_uri,
            safe_mode: WasmSafeMode::Secure,
            allowed_schemes: options.allowed_schemes,
            attributes: options.attributes,
            enable_includes: options.enable_includes,
            max_include_depth: options.max_include_depth,
            max_includes: options.max_includes,
            max_total_bytes: options.max_total_bytes,
            max_expanded_nodes: options.max_expanded_nodes,
            max_source_map_segments: options.max_source_map_segments,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum WasmSafeMode {
    Unsafe,
    Server,
    Safe,
    #[default]
    Secure,
}

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
