use std::collections::BTreeMap;

use adocweave::SourceId;
use adocweave::preprocess::{PreprocessOptions, ResourceDocument, ResourceSnapshot, SafeMode};
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

pub(crate) fn resource_snapshot(resources: BTreeMap<String, WasmResource>) -> ResourceSnapshot {
    let mut snapshot = ResourceSnapshot::default();
    for (target, resource) in resources {
        snapshot.insert(
            target,
            ResourceDocument {
                source_id: SourceId::new(resource.source_id),
                source: resource.source.into(),
            },
        );
    }
    snapshot
}

pub(crate) fn to_core_options(
    source_id: Option<SourceId>,
    options: WasmPreprocessOptions,
) -> PreprocessOptions {
    PreprocessOptions {
        source_id,
        base_uri: options.base_uri,
        safe_mode: match options.safe_mode {
            WasmSafeMode::Unsafe => SafeMode::Unsafe,
            WasmSafeMode::Server => SafeMode::Server,
            WasmSafeMode::Safe => SafeMode::Safe,
            WasmSafeMode::Secure => SafeMode::Secure,
        },
        allowed_schemes: options
            .allowed_schemes
            .into_iter()
            .map(|scheme| scheme.to_ascii_lowercase())
            .collect(),
        attributes: options.attributes,
        enable_includes: options.enable_includes,
        max_include_depth: options.max_include_depth,
        max_includes: options.max_includes,
        max_total_bytes: options.max_total_bytes,
        max_expanded_nodes: options.max_expanded_nodes,
        max_source_map_segments: options.max_source_map_segments,
    }
}
