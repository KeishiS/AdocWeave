//! Source offsets, ranges, and line-based position conversion.

use std::error::Error;
use std::fmt;

pub use crate::source_document::SourceDocument;

/// A zero-based offset in the original UTF-8 byte sequence.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct TextSize(u32);

impl TextSize {
    pub const ZERO: Self = Self(0);

    /// Creates an offset when it fits in the source model.
    pub fn new(value: usize) -> Result<Self, PositionError> {
        u32::try_from(value)
            .map(Self)
            .map_err(|_| PositionError::SourceTooLarge { length: value })
    }

    pub const fn to_u32(self) -> u32 {
        self.0
    }

    pub const fn to_usize(self) -> usize {
        self.0 as usize
    }
}

/// A half-open range `[start, end)` in the original UTF-8 byte sequence.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct TextRange {
    start: TextSize,
    end: TextSize,
}

impl TextRange {
    pub fn new(start: TextSize, end: TextSize) -> Result<Self, PositionError> {
        if start <= end {
            Ok(Self { start, end })
        } else {
            Err(PositionError::ReversedRange { start, end })
        }
    }

    pub const fn start(self) -> TextSize {
        self.start
    }

    pub const fn end(self) -> TextSize {
        self.end
    }

    pub const fn is_empty(self) -> bool {
        self.start.0 == self.end.0
    }

    pub const fn len(self) -> TextSize {
        TextSize(self.end.0 - self.start.0)
    }
}

/// The unit used by the `character` field of an LSP-style position.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PositionEncoding {
    Utf8,
    Utf16,
}

/// A zero-based line and character position.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

pub(crate) fn utf16_character_to_byte(
    content: &str,
    position: Position,
) -> Result<usize, PositionError> {
    let requested = position.character as usize;
    let mut utf16_offset = 0;

    for (byte_offset, character) in content.char_indices() {
        if utf16_offset == requested {
            return Ok(byte_offset);
        }

        utf16_offset += character.len_utf16();
        if utf16_offset > requested {
            return Err(PositionError::InvalidCharacterBoundary {
                position,
                encoding: PositionEncoding::Utf16,
            });
        }
    }

    if utf16_offset == requested {
        Ok(content.len())
    } else {
        Err(PositionError::CharacterOutOfBounds {
            position,
            line_length: u32::try_from(utf16_offset).expect("source length limits the line length"),
            encoding: PositionEncoding::Utf16,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PositionError {
    SourceTooLarge {
        length: usize,
    },
    ReversedRange {
        start: TextSize,
        end: TextSize,
    },
    OffsetOutOfBounds {
        offset: TextSize,
        source_len: TextSize,
    },
    InvalidCharBoundary {
        offset: TextSize,
    },
    InsideLineEnding {
        offset: TextSize,
    },
    LineOutOfBounds {
        line: u32,
        line_count: u32,
    },
    CharacterOutOfBounds {
        position: Position,
        line_length: u32,
        encoding: PositionEncoding,
    },
    InvalidCharacterBoundary {
        position: Position,
        encoding: PositionEncoding,
    },
}

impl fmt::Display for PositionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl Error for PositionError {}

#[cfg(test)]
mod tests {
    use super::{Position, PositionEncoding, PositionError, SourceDocument, TextRange, TextSize};

    fn size(value: usize) -> TextSize {
        TextSize::new(value).expect("small test offset")
    }

    #[test]
    fn source_position_range_is_half_open_and_ordered() {
        let range = TextRange::new(size(2), size(5)).expect("ordered range");

        assert_eq!(range.start(), size(2));
        assert_eq!(range.end(), size(5));
        assert_eq!(range.len(), size(3));
        assert!(!range.is_empty());
        assert_eq!(
            TextRange::new(size(5), size(2)),
            Err(PositionError::ReversedRange {
                start: size(5),
                end: size(2),
            })
        );
    }

    #[test]
    fn source_document_converts_ascii_japanese_emoji_and_combining_characters() {
        let source = "a日😀e\u{301}\n";
        let index = SourceDocument::new(source).expect("valid source");

        let cases = [
            (0, 0, 0),
            (1, 1, 1),
            (4, 4, 2),
            (8, 8, 4),
            (9, 9, 5),
            (11, 11, 6),
        ];
        for (byte, utf8, utf16) in cases {
            assert_eq!(
                index.offset_to_position(size(byte), PositionEncoding::Utf8),
                Ok(Position {
                    line: 0,
                    character: utf8,
                })
            );
            assert_eq!(
                index.offset_to_position(size(byte), PositionEncoding::Utf16),
                Ok(Position {
                    line: 0,
                    character: utf16,
                })
            );
        }
    }

    #[test]
    fn source_document_handles_lf_crlf_and_document_end() {
        let index = SourceDocument::new("a\r\nb\n").expect("valid source");

        assert_eq!(index.line_count(), 3);
        assert_eq!(
            index.offset_to_position(size(1), PositionEncoding::Utf16),
            Ok(Position {
                line: 0,
                character: 1,
            })
        );
        assert_eq!(
            index.offset_to_position(size(2), PositionEncoding::Utf16),
            Err(PositionError::InsideLineEnding { offset: size(2) })
        );
        assert_eq!(
            index.offset_to_position(size(3), PositionEncoding::Utf16),
            Ok(Position {
                line: 1,
                character: 0,
            })
        );
        assert_eq!(
            index.offset_to_position(size(5), PositionEncoding::Utf16),
            Ok(Position {
                line: 2,
                character: 0,
            })
        );
    }

    #[test]
    fn line_lengths_use_the_requested_position_encoding() {
        let index = SourceDocument::new("a😀\r\nb").expect("valid source");

        assert_eq!(index.line_length(0, PositionEncoding::Utf8), Ok(5));
        assert_eq!(index.line_length(0, PositionEncoding::Utf16), Ok(3));
        assert_eq!(index.line_length(1, PositionEncoding::Utf8), Ok(1));
    }

    #[test]
    fn source_document_keeps_bom_nul_and_tab_in_the_source() {
        let source = "\u{feff}\0\tX";
        let index = SourceDocument::new(source).expect("valid source");

        assert_eq!(
            index.offset_to_position(size(source.len()), PositionEncoding::Utf8),
            Ok(Position {
                line: 0,
                character: 6,
            })
        );
        assert_eq!(
            index.offset_to_position(size(source.len()), PositionEncoding::Utf16),
            Ok(Position {
                line: 0,
                character: 4,
            })
        );
    }

    #[test]
    fn source_document_rejects_offsets_and_positions_inside_characters() {
        let index = SourceDocument::new("😀").expect("valid source");

        assert_eq!(
            index.offset_to_position(size(1), PositionEncoding::Utf8),
            Err(PositionError::InvalidCharBoundary { offset: size(1) })
        );
        assert_eq!(
            index.position_to_offset(
                Position {
                    line: 0,
                    character: 1,
                },
                PositionEncoding::Utf8,
            ),
            Err(PositionError::InvalidCharacterBoundary {
                position: Position {
                    line: 0,
                    character: 1,
                },
                encoding: PositionEncoding::Utf8,
            })
        );
        assert_eq!(
            index.position_to_offset(
                Position {
                    line: 0,
                    character: 1,
                },
                PositionEncoding::Utf16,
            ),
            Err(PositionError::InvalidCharacterBoundary {
                position: Position {
                    line: 0,
                    character: 1,
                },
                encoding: PositionEncoding::Utf16,
            })
        );
    }

    #[test]
    fn source_document_round_trips_valid_positions_for_both_encodings() {
        let source = "日本語\r\nemoji 😀\n";
        let index = SourceDocument::new(source).expect("valid source");

        for offset in 0..=source.len() {
            if !source.is_char_boundary(offset) || offset == 10 {
                continue;
            }
            for encoding in [PositionEncoding::Utf8, PositionEncoding::Utf16] {
                let position = index
                    .offset_to_position(size(offset), encoding)
                    .expect("valid byte offset");
                assert_eq!(
                    index.position_to_offset(position, encoding),
                    Ok(size(offset))
                );
            }
        }
    }
}
