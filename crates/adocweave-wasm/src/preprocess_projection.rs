//! Meaning projection from a core preprocessing result to generated wire values.

use adocweave::VERSION;
use adocweave::preprocess::{PreprocessedDocument, SourceMapping};

use crate::{WasmPreprocessResponse, WasmSourceMapSegment, WasmSourceMapping};

pub(crate) fn project(document: PreprocessedDocument) -> WasmPreprocessResponse {
    let source_map = document
        .source_map()
        .iter()
        .map(|segment| WasmSourceMapSegment {
            output_start: segment.output_range.start().to_u32(),
            output_end: segment.output_range.end().to_u32(),
            source_id: segment
                .origin
                .source_id
                .as_ref()
                .map(|source_id| source_id.as_str().to_owned()),
            source_start: segment.origin.range.start().to_u32(),
            source_end: segment.origin.range.end().to_u32(),
            mapping: match segment.mapping {
                SourceMapping::Identity => WasmSourceMapping::Identity,
                SourceMapping::WholeOrigin => WasmSourceMapping::WholeOrigin,
            },
        })
        .collect();
    WasmPreprocessResponse {
        package_version: VERSION.to_owned(),
        source: document.source,
        source_map,
    }
}
