use super::{
    Directive, ExpandedOffset, ExpandedRange, OriginRange, PreprocessNotice, PreprocessedDocument,
    SourceId, SourceMapInvariantError, SourceMapSegment, SourceMapping, SourceOrigin, TextRange,
    TextSize,
};

pub(super) struct SourceMapBuilder {
    source: String,
    segments: Vec<SourceMapSegment>,
    max_bytes: usize,
    max_segments: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SourceMapBuildError {
    ByteLimit,
    SegmentLimit,
}

impl SourceMapBuilder {
    pub(super) fn new(max_bytes: u32, max_segments: u32) -> Self {
        Self {
            source: String::new(),
            segments: Vec::new(),
            max_bytes: max_bytes as usize,
            max_segments: max_segments as usize,
        }
    }

    pub(super) fn append(
        &mut self,
        value: &str,
        source_id: Option<SourceId>,
        origin_range: TextRange,
        mapping: SourceMapping,
    ) -> Result<(), SourceMapBuildError> {
        let start = self.source.len();
        let end = start.saturating_add(value.len());
        if end > self.max_bytes {
            return Err(SourceMapBuildError::ByteLimit);
        }
        if start < end && self.segments.len() >= self.max_segments {
            return Err(SourceMapBuildError::SegmentLimit);
        }

        self.source.push_str(value);
        if start < end {
            self.segments.push(SourceMapSegment {
                output_range: ExpandedRange::new(text_range(start, end)),
                origin: SourceOrigin {
                    source_id,
                    range: OriginRange::new(origin_range),
                },
                mapping,
            });
        }
        Ok(())
    }

    pub(super) fn finish(
        self,
        directives: Vec<Directive>,
        notices: Vec<PreprocessNotice>,
    ) -> Result<PreprocessedDocument, SourceMapInvariantError> {
        PreprocessedDocument::from_parts(self.source, self.segments, directives, notices)
    }
}

impl PreprocessedDocument {
    pub(super) fn from_parts(
        source: String,
        source_map: Vec<SourceMapSegment>,
        directives: Vec<Directive>,
        notices: Vec<PreprocessNotice>,
    ) -> Result<Self, SourceMapInvariantError> {
        let source_end = TextSize::new(source.len()).map_err(|_| SourceMapInvariantError)?;
        let mut previous_end = TextSize::ZERO;
        for segment in &source_map {
            let output_range = segment.output_range;
            let output_length = output_range
                .end()
                .to_u32()
                .saturating_sub(output_range.start().to_u32());
            let origin_length = segment
                .origin
                .range
                .end()
                .to_u32()
                .saturating_sub(segment.origin.range.start().to_u32());
            if output_range.is_empty()
                || output_range.start() < previous_end
                || output_range.end() > source_end
                || (segment.mapping == SourceMapping::Identity && output_length != origin_length)
            {
                return Err(SourceMapInvariantError);
            }
            previous_end = output_range.end();
        }
        Ok(Self {
            source,
            source_map,
            directives,
            notices,
        })
    }

    pub fn source_map(&self) -> &[SourceMapSegment] {
        &self.source_map
    }

    pub fn origin_at(&self, output_offset: ExpandedOffset) -> Option<&SourceOrigin> {
        let output_offset = output_offset.text_size();
        let index = self
            .source_map
            .partition_point(|segment| segment.output_range.end() <= output_offset);
        self.source_map
            .get(index)
            .filter(|segment| segment.output_range.start() <= output_offset)
            .map(|segment| &segment.origin)
    }

    /// Maps an output range to the originating source segment.
    ///
    /// When a range crosses include boundaries, the origin containing its
    /// start is returned. Consumers that need exact pieces should inspect
    /// `source_map` directly.
    pub fn origin_for_range(&self, output_range: ExpandedRange) -> Option<&SourceOrigin> {
        if let Some(origin) = self.origin_at(ExpandedOffset::new(output_range.start())) {
            return Some(origin);
        }
        if !output_range.is_empty() {
            return None;
        }
        self.source_map
            .iter()
            .rev()
            .find(|segment| segment.output_range.end() == output_range.start())
            .map(|segment| &segment.origin)
    }

    /// Projects an expanded range into all originating source ranges.
    ///
    /// Adjacent pieces in the same source are merged. For an unchanged segment,
    /// the relative byte range is preserved. A transformed segment (for example
    /// `indent` or `leveloffset`) conservatively maps to its complete source line.
    pub fn origins_for_range(&self, output_range: ExpandedRange) -> Vec<SourceOrigin> {
        if output_range.is_empty() {
            let segment = self
                .source_map
                .iter()
                .find(|segment| {
                    segment.output_range.start() <= output_range.start()
                        && output_range.start() < segment.output_range.end()
                })
                .or_else(|| {
                    self.source_map
                        .last()
                        .filter(|segment| segment.output_range.end() == output_range.start())
                });
            let Some(segment) = segment else {
                return Vec::new();
            };
            let range = project_range(segment, output_range.start(), output_range.end());
            return vec![SourceOrigin {
                source_id: segment.origin.source_id.clone(),
                range: OriginRange::new(range),
            }];
        }
        let mut origins: Vec<SourceOrigin> = Vec::new();
        let first = self
            .source_map
            .partition_point(|segment| segment.output_range.end() <= output_range.start());
        for segment in &self.source_map[first..] {
            if output_range.end() <= segment.output_range.start() {
                break;
            }
            let start = segment
                .output_range
                .start()
                .to_u32()
                .max(output_range.start().to_u32());
            let end = segment
                .output_range
                .end()
                .to_u32()
                .min(output_range.end().to_u32());
            if start >= end {
                continue;
            }
            let range = project_range(
                segment,
                TextSize::new(start as usize).expect("projected start is bounded"),
                TextSize::new(end as usize).expect("projected end is bounded"),
            );
            let origin = SourceOrigin {
                source_id: segment.origin.source_id.clone(),
                range: OriginRange::new(range),
            };
            let merged = if let Some(previous) = origins.last_mut() {
                if previous.source_id == origin.source_id
                    && previous.range.end() == origin.range.start()
                {
                    previous.range = OriginRange::new(
                        TextRange::new(previous.range.start(), origin.range.end())
                            .expect("merged source range is ordered"),
                    );
                    true
                } else {
                    false
                }
            } else {
                false
            };
            if !merged {
                origins.push(origin);
            }
        }
        origins
    }

    pub(super) fn origins_for_empty_range_within(
        &self,
        output_range: ExpandedRange,
        containing_range: ExpandedRange,
    ) -> Vec<SourceOrigin> {
        debug_assert!(output_range.is_empty());
        let Some(segment) = self.source_map.iter().find(|segment| {
            segment.output_range.start() <= output_range.start()
                && output_range.start() <= segment.output_range.end()
                && segment.output_range.start() < containing_range.end()
                && containing_range.start() < segment.output_range.end()
        }) else {
            return self.origins_for_range(output_range);
        };
        vec![SourceOrigin {
            source_id: segment.origin.source_id.clone(),
            range: OriginRange::new(project_range(
                segment,
                output_range.start(),
                output_range.end(),
            )),
        }]
    }

    pub(super) fn mapping_is_identity(&self, output_range: ExpandedRange) -> bool {
        if output_range.is_empty() {
            return false;
        }
        let index = self
            .source_map
            .partition_point(|segment| segment.output_range.end() <= output_range.start());
        self.source_map.get(index).is_some_and(|segment| {
            segment.mapping == SourceMapping::Identity
                && segment.output_range.start() <= output_range.start()
                && output_range.end() <= segment.output_range.end()
        })
    }
}

fn project_range(segment: &SourceMapSegment, start: TextSize, end: TextSize) -> TextRange {
    if segment.mapping == SourceMapping::WholeOrigin {
        return segment.origin.range.text_range();
    }
    let relative_start = start
        .to_u32()
        .saturating_sub(segment.output_range.start().to_u32());
    let relative_end = end
        .to_u32()
        .saturating_sub(segment.output_range.start().to_u32());
    TextRange::new(
        TextSize::new(segment.origin.range.start().to_usize() + relative_start as usize)
            .expect("projected source start is bounded"),
        TextSize::new(segment.origin.range.start().to_usize() + relative_end as usize)
            .expect("projected source end is bounded"),
    )
    .expect("projected source range is ordered")
}

fn text_range(start: usize, end: usize) -> TextRange {
    TextRange::new(
        TextSize::new(start).expect("source map offset is bounded"),
        TextSize::new(end).expect("source map offset is bounded"),
    )
    .expect("source map range is ordered")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin(source: &str, start: usize, end: usize) -> (Option<SourceId>, TextRange) {
        (Some(SourceId::new(source)), text_range(start, end))
    }

    fn contains(outer: TextRange, inner: TextRange) -> bool {
        outer.start() <= inner.start() && inner.end() <= outer.end()
    }

    #[test]
    fn builder_rejects_limits_without_partially_mutating_the_snapshot() {
        let mut bytes = SourceMapBuilder::new(3, 2);
        let (source_id, range) = origin("root", 0, 4);
        assert_eq!(
            bytes.append("four", source_id, range, SourceMapping::Identity),
            Err(SourceMapBuildError::ByteLimit)
        );
        let document = bytes.finish(Vec::new(), Vec::new()).expect("empty map");
        assert!(document.source.is_empty());
        assert!(document.source_map().is_empty());

        let mut segments = SourceMapBuilder::new(8, 1);
        let (first_id, first_range) = origin("root", 0, 1);
        segments
            .append("a", first_id, first_range, SourceMapping::Identity)
            .expect("first segment");
        let (second_id, second_range) = origin("root", 1, 2);
        assert_eq!(
            segments.append("b", second_id, second_range, SourceMapping::Identity),
            Err(SourceMapBuildError::SegmentLimit)
        );
        let document = segments
            .finish(Vec::new(), Vec::new())
            .expect("bounded map");
        assert_eq!(document.source, "a");
        assert_eq!(document.source_map().len(), 1);
    }

    #[test]
    fn snapshot_rejects_unsorted_overlapping_outside_empty_and_mismatched_identity_segments() {
        let segment =
            |output_start, output_end, origin_start, origin_end, mapping| SourceMapSegment {
                output_range: ExpandedRange::new(text_range(output_start, output_end)),
                origin: SourceOrigin {
                    source_id: Some(SourceId::new("root")),
                    range: OriginRange::new(text_range(origin_start, origin_end)),
                },
                mapping,
            };
        for source_map in [
            vec![
                segment(1, 2, 1, 2, SourceMapping::Identity),
                segment(0, 1, 0, 1, SourceMapping::Identity),
            ],
            vec![
                segment(0, 2, 0, 2, SourceMapping::Identity),
                segment(1, 2, 1, 2, SourceMapping::Identity),
            ],
            vec![segment(0, 3, 0, 3, SourceMapping::Identity)],
            vec![segment(1, 1, 1, 1, SourceMapping::Identity)],
            vec![segment(0, 2, 4, 5, SourceMapping::Identity)],
        ] {
            assert!(
                PreprocessedDocument::from_parts(
                    "ab".to_owned(),
                    source_map,
                    Vec::new(),
                    Vec::new()
                )
                .is_err()
            );
        }
        assert!(
            PreprocessedDocument::from_parts(
                "ab".to_owned(),
                vec![segment(0, 2, 4, 5, SourceMapping::WholeOrigin)],
                Vec::new(),
                Vec::new()
            )
            .is_ok()
        );
    }

    #[test]
    fn every_bounded_query_projects_inside_identity_origins() {
        let document = PreprocessedDocument::from_parts(
            "abcdef".to_owned(),
            vec![
                SourceMapSegment {
                    output_range: ExpandedRange::new(text_range(0, 3)),
                    origin: SourceOrigin {
                        source_id: Some(SourceId::new("first")),
                        range: OriginRange::new(text_range(10, 13)),
                    },
                    mapping: SourceMapping::Identity,
                },
                SourceMapSegment {
                    output_range: ExpandedRange::new(text_range(3, 6)),
                    origin: SourceOrigin {
                        source_id: Some(SourceId::new("second")),
                        range: OriginRange::new(text_range(20, 23)),
                    },
                    mapping: SourceMapping::Identity,
                },
            ],
            Vec::new(),
            Vec::new(),
        )
        .expect("valid map");

        for start in 0..=6 {
            for end in start..=6 {
                let projected =
                    document.origins_for_range(ExpandedRange::new(text_range(start, end)));
                assert_eq!(
                    projected,
                    document.origins_for_range(ExpandedRange::new(text_range(start, end)))
                );
                assert!(
                    projected
                        .iter()
                        .all(|origin| match origin.source_id.as_ref() {
                            Some(source_id) if source_id.as_str() == "first" => {
                                contains(text_range(10, 13), origin.range.text_range())
                            }
                            Some(source_id) if source_id.as_str() == "second" => {
                                contains(text_range(20, 23), origin.range.text_range())
                            }
                            _ => false,
                        })
                );
            }
        }
    }
}
