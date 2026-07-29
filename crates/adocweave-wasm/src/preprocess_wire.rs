use std::collections::BTreeMap;

use adocweave::SourceId;
use adocweave::preprocess::{PreprocessOptions, ResourceDocument, ResourceSnapshot, SafeMode};

pub use crate::preprocess_wire_generated::{
    WasmAnalysisPreprocessInput, WasmError, WasmPreprocessOptions, WasmPreprocessRequest,
    WasmPreprocessResponse, WasmResource, WasmSafeMode, WasmSourceMapSegment, WasmSourceMapping,
};

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
        max_attribute_expansion_depth: options.max_attribute_expansion_depth,
        max_attribute_expansion_bytes: options.max_attribute_expansion_bytes,
    }
}
