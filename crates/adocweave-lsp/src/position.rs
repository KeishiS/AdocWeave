use adocweave::text::{
    Position as CorePosition, PositionEncoding as CorePositionEncoding, SourceDocument,
    TextRange as CoreTextRange,
};
use async_lsp::lsp_types as lsp;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PositionEncoding {
    Utf8,
    Utf16,
}

impl PositionEncoding {
    pub(crate) const fn core(self) -> CorePositionEncoding {
        match self {
            Self::Utf8 => CorePositionEncoding::Utf8,
            Self::Utf16 => CorePositionEncoding::Utf16,
        }
    }

    pub(crate) fn lsp(self) -> lsp::PositionEncodingKind {
        match self {
            Self::Utf8 => lsp::PositionEncodingKind::UTF8,
            Self::Utf16 => lsp::PositionEncodingKind::UTF16,
        }
    }
}

pub(crate) fn negotiate_encoding(params: &lsp::InitializeParams) -> PositionEncoding {
    if params
        .capabilities
        .general
        .as_ref()
        .and_then(|general| general.position_encodings.as_ref())
        .is_some_and(|encodings| encodings.contains(&lsp::PositionEncodingKind::UTF8))
    {
        PositionEncoding::Utf8
    } else {
        PositionEncoding::Utf16
    }
}

pub(crate) fn request_offset(
    source_document: &SourceDocument,
    position: lsp::Position,
    encoding: PositionEncoding,
) -> Result<u32, String> {
    if position.line >= source_document.line_count() {
        return Err("position.line is outside the document".to_owned());
    }
    source_document
        .position_to_offset(
            CorePosition {
                line: position.line,
                character: position.character,
            },
            encoding.core(),
        )
        .map(|offset| offset.to_u32())
        .map_err(|error| error.to_string())
}

pub(crate) fn range_to_lsp(
    range: CoreTextRange,
    source_document: &SourceDocument,
    encoding: PositionEncoding,
) -> Result<lsp::Range, String> {
    let start = source_document
        .offset_to_position(range.start(), encoding.core())
        .map_err(|error| error.to_string())?;
    let end = source_document
        .offset_to_position(range.end(), encoding.core())
        .map_err(|error| error.to_string())?;
    Ok(lsp::Range::new(
        lsp::Position::new(start.line, start.character),
        lsp::Position::new(end.line, end.character),
    ))
}

pub(crate) fn ranges_intersect(left: lsp::Range, right: lsp::Range) -> bool {
    if left.start == left.end {
        return point_in_range(left.start, right);
    }
    if right.start == right.end {
        return point_in_range(right.start, left);
    }
    position_lt(left.start, right.end) && position_lt(right.start, left.end)
}

fn point_in_range(point: lsp::Position, range: lsp::Range) -> bool {
    if range.start == range.end {
        point == range.start
    } else {
        position_le(range.start, point) && position_lt(point, range.end)
    }
}

fn position_le(left: lsp::Position, right: lsp::Position) -> bool {
    (left.line, left.character) <= (right.line, right.character)
}

fn position_lt(left: lsp::Position, right: lsp::Position) -> bool {
    (left.line, left.character) < (right.line, right.character)
}

/// Returns whether a source offset is inside a half-open text range.
pub(crate) fn range_contains_offset(range: CoreTextRange, offset: u32) -> bool {
    range.start().to_u32() <= offset && offset < range.end().to_u32()
}

/// Returns whether a cursor can complete content in or immediately after a range.
pub(crate) fn cursor_touches_range(range: CoreTextRange, offset: u32) -> bool {
    range.start().to_u32() <= offset && offset <= range.end().to_u32()
}

#[cfg(test)]
mod tests {
    use super::*;
    use adocweave::text::{TextRange, TextSize};
    use serde_json::json;

    #[test]
    fn encoding_negotiation_prefers_utf8_and_defaults_to_utf16() {
        let utf8 = serde_json::from_value(json!({
            "processId": null,
            "capabilities": {
                "general": {
                    "positionEncodings": ["utf-16", "utf-8"]
                }
            }
        }))
        .expect("initialize params");
        let default = serde_json::from_value(json!({
            "processId": null,
            "capabilities": {}
        }))
        .expect("initialize params");

        assert_eq!(negotiate_encoding(&utf8), PositionEncoding::Utf8);
        assert_eq!(negotiate_encoding(&default), PositionEncoding::Utf16);
    }

    #[test]
    fn unicode_positions_round_trip_for_each_encoding() {
        let document = SourceDocument::new("日😀x\n").expect("source document");
        let range = TextRange::new(
            TextSize::new("日😀".len()).expect("start"),
            TextSize::new("日😀x".len()).expect("end"),
        )
        .expect("range");

        for (encoding, start, end) in [
            (PositionEncoding::Utf8, 7, 8),
            (PositionEncoding::Utf16, 3, 4),
        ] {
            let converted = range_to_lsp(range, &document, encoding).expect("LSP range");
            assert_eq!(
                converted,
                lsp::Range::new(lsp::Position::new(0, start), lsp::Position::new(0, end),)
            );
            assert_eq!(
                request_offset(&document, converted.start, encoding).expect("offset"),
                range.start().to_u32(),
            );
        }
    }

    #[test]
    fn range_intersection_uses_half_open_ranges_and_accepts_equal_points() {
        let range = lsp::Range::new(lsp::Position::new(1, 2), lsp::Position::new(1, 5));

        assert!(ranges_intersect(
            range,
            lsp::Range::new(lsp::Position::new(1, 4), lsp::Position::new(1, 6)),
        ));
        assert!(!ranges_intersect(
            range,
            lsp::Range::new(lsp::Position::new(1, 5), lsp::Position::new(1, 6)),
        ));
        assert!(ranges_intersect(
            range,
            lsp::Range::new(lsp::Position::new(1, 3), lsp::Position::new(1, 3)),
        ));
    }

    #[test]
    fn text_containment_is_half_open_while_completion_accepts_the_end_cursor() {
        let range = TextRange::new(
            TextSize::new(2).expect("start"),
            TextSize::new(5).expect("end"),
        )
        .expect("range");

        assert!(range_contains_offset(range, 2));
        assert!(range_contains_offset(range, 4));
        assert!(!range_contains_offset(range, 5));
        assert!(cursor_touches_range(range, 5));
    }
}
