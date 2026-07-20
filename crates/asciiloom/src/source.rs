//! Source offsets, ranges, and line-based position conversion.

use std::error::Error;
use std::fmt;

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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

#[derive(Clone, Copy, Debug)]
struct Line {
    start: usize,
    content_end: usize,
    full_end: usize,
}

/// Converts between source byte offsets and zero-based UTF-8 or UTF-16 positions.
#[derive(Debug)]
pub struct LineIndex<'source> {
    source: &'source str,
    lines: Vec<Line>,
}

impl<'source> LineIndex<'source> {
    pub fn new(source: &'source str) -> Result<Self, PositionError> {
        TextSize::new(source.len())?;

        let bytes = source.as_bytes();
        let mut lines = Vec::new();
        let mut line_start = 0;
        let mut cursor = 0;

        while cursor < bytes.len() {
            match bytes[cursor] {
                b'\r' if bytes.get(cursor + 1) == Some(&b'\n') => {
                    lines.push(Line {
                        start: line_start,
                        content_end: cursor,
                        full_end: cursor + 2,
                    });
                    cursor += 2;
                    line_start = cursor;
                }
                b'\n' => {
                    lines.push(Line {
                        start: line_start,
                        content_end: cursor,
                        full_end: cursor + 1,
                    });
                    cursor += 1;
                    line_start = cursor;
                }
                _ => cursor += 1,
            }
        }

        lines.push(Line {
            start: line_start,
            content_end: bytes.len(),
            full_end: bytes.len(),
        });

        Ok(Self { source, lines })
    }

    pub fn line_count(&self) -> u32 {
        u32::try_from(self.lines.len()).expect("source length limits the number of lines")
    }

    pub fn offset_to_position(
        &self,
        offset: TextSize,
        encoding: PositionEncoding,
    ) -> Result<Position, PositionError> {
        let offset = offset.to_usize();
        if offset > self.source.len() {
            return Err(PositionError::OffsetOutOfBounds {
                offset: TextSize::new(offset)?,
                source_len: TextSize::new(self.source.len())?,
            });
        }
        if !self.source.is_char_boundary(offset) {
            return Err(PositionError::InvalidCharBoundary {
                offset: TextSize::new(offset)?,
            });
        }

        let line_number = self
            .lines
            .partition_point(|line| line.full_end <= offset && line.full_end != line.content_end);
        let line = self
            .lines
            .get(line_number)
            .expect("an in-bounds offset belongs to a line");

        if offset > line.content_end {
            return Err(PositionError::InsideLineEnding {
                offset: TextSize::new(offset)?,
            });
        }

        let prefix = &self.source[line.start..offset];
        let character = match encoding {
            PositionEncoding::Utf8 => prefix.len(),
            PositionEncoding::Utf16 => prefix.encode_utf16().count(),
        };

        Ok(Position {
            line: u32::try_from(line_number).expect("source length limits the line number"),
            character: u32::try_from(character).expect("source length limits the character"),
        })
    }

    pub fn position_to_offset(
        &self,
        position: Position,
        encoding: PositionEncoding,
    ) -> Result<TextSize, PositionError> {
        let Some(line) = self.lines.get(position.line as usize) else {
            return Err(PositionError::LineOutOfBounds {
                line: position.line,
                line_count: self.line_count(),
            });
        };
        let content = &self.source[line.start..line.content_end];
        let requested = position.character as usize;

        let relative_offset = match encoding {
            PositionEncoding::Utf8 => {
                if requested > content.len() {
                    return Err(PositionError::CharacterOutOfBounds {
                        position,
                        line_length: u32::try_from(content.len())
                            .expect("source length limits the line length"),
                        encoding,
                    });
                }
                if !content.is_char_boundary(requested) {
                    return Err(PositionError::InvalidCharacterBoundary { position, encoding });
                }
                requested
            }
            PositionEncoding::Utf16 => utf16_character_to_byte(content, position)?,
        };

        TextSize::new(line.start + relative_offset)
    }
}

fn utf16_character_to_byte(content: &str, position: Position) -> Result<usize, PositionError> {
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
    use super::{LineIndex, Position, PositionEncoding, PositionError, TextRange, TextSize};

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
    fn line_index_converts_ascii_japanese_emoji_and_combining_characters() {
        let source = "a日😀e\u{301}\n";
        let index = LineIndex::new(source).expect("valid source");

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
    fn line_index_handles_lf_crlf_and_document_end() {
        let index = LineIndex::new("a\r\nb\n").expect("valid source");

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
    fn line_index_keeps_bom_nul_and_tab_in_the_source() {
        let source = "\u{feff}\0\tX";
        let index = LineIndex::new(source).expect("valid source");

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
    fn line_index_rejects_offsets_and_positions_inside_characters() {
        let index = LineIndex::new("😀").expect("valid source");

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
    fn line_index_round_trips_valid_positions_for_both_encodings() {
        let source = "日本語\r\nemoji 😀\n";
        let index = LineIndex::new(source).expect("valid source");

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
