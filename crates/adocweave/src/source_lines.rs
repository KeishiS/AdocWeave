//! Lossless recognition of source lines and their line endings.

use std::sync::Arc;

use crate::source::{PositionError, TextRange, TextSize};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LineEnding {
    None,
    Lf,
    CrLf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceLine {
    content: TextRange,
    ending: TextRange,
    full: TextRange,
    ending_kind: LineEnding,
}

impl SourceLine {
    pub const fn content_range(self) -> TextRange {
        self.content
    }

    pub const fn ending_range(self) -> TextRange {
        self.ending
    }

    pub const fn full_range(self) -> TextRange {
        self.full
    }

    pub const fn ending(self) -> LineEnding {
        self.ending_kind
    }
}

/// Token categories retained by the lossless syntax layer.
///
/// The initial lexer emits text, whitespace, comments, and line endings.
/// Delimiters and unsupported regions are reserved for later grammar issues.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LosslessTokenKind {
    Text,
    Whitespace,
    Comment,
    Delimiter,
    Unsupported,
    LineEnding(LineEnding),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LosslessToken {
    pub kind: LosslessTokenKind,
    pub range: TextRange,
}

/// An owned line and token view of the original UTF-8 source.
#[derive(Debug)]
pub struct SourceLines {
    source: Arc<str>,
    lines: Vec<SourceLine>,
    tokens: Vec<LosslessToken>,
}

impl SourceLines {
    pub fn new(source: &str) -> Result<Self, PositionError> {
        Self::from_shared(Arc::from(source))
    }

    pub fn from_shared(source: Arc<str>) -> Result<Self, PositionError> {
        let source_text = source.as_ref();
        TextSize::new(source_text.len())?;

        let mut lines = Vec::new();
        let mut tokens = Vec::new();
        let bytes = source_text.as_bytes();
        let mut line_start = 0;
        let mut cursor = 0;

        while cursor < bytes.len() {
            let (content_end, full_end, ending) = match bytes[cursor] {
                b'\r' if bytes.get(cursor + 1) == Some(&b'\n') => {
                    (cursor, cursor + 2, LineEnding::CrLf)
                }
                b'\n' => (cursor, cursor + 1, LineEnding::Lf),
                _ => {
                    cursor += 1;
                    continue;
                }
            };

            push_line(
                source_text,
                &mut lines,
                &mut tokens,
                line_start,
                content_end,
                full_end,
                ending,
            )?;
            cursor = full_end;
            line_start = full_end;
        }

        push_line(
            source_text,
            &mut lines,
            &mut tokens,
            line_start,
            source_text.len(),
            source_text.len(),
            LineEnding::None,
        )?;

        Ok(Self {
            source,
            lines,
            tokens,
        })
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn lines(&self) -> &[SourceLine] {
        &self.lines
    }

    pub fn tokens(&self) -> &[LosslessToken] {
        &self.tokens
    }

    pub fn text(&self, range: TextRange) -> Option<&str> {
        self.source
            .get(range.start().to_usize()..range.end().to_usize())
    }

    /// Reconstructs the original source solely from the token ranges.
    pub fn reconstruct(&self) -> String {
        let mut output = String::with_capacity(self.source.len());
        for token in &self.tokens {
            output.push_str(
                self.text(token.range)
                    .expect("lexer-generated ranges are valid UTF-8 boundaries"),
            );
        }
        output
    }
}

fn push_line(
    source: &str,
    lines: &mut Vec<SourceLine>,
    tokens: &mut Vec<LosslessToken>,
    start: usize,
    content_end: usize,
    full_end: usize,
    ending: LineEnding,
) -> Result<(), PositionError> {
    let content = text_range(start, content_end)?;
    let ending_range = text_range(content_end, full_end)?;
    let full = text_range(start, full_end)?;
    lines.push(SourceLine {
        content,
        ending: ending_range,
        full,
        ending_kind: ending,
    });

    push_content_tokens(source, tokens, start, content_end)?;
    if ending != LineEnding::None {
        tokens.push(LosslessToken {
            kind: LosslessTokenKind::LineEnding(ending),
            range: ending_range,
        });
    }
    Ok(())
}

fn push_content_tokens(
    source: &str,
    tokens: &mut Vec<LosslessToken>,
    start: usize,
    end: usize,
) -> Result<(), PositionError> {
    let content = &source[start..end];
    let leading_whitespace = content
        .bytes()
        .take_while(|byte| matches!(byte, b' ' | b'\t'))
        .count();

    if content[leading_whitespace..].starts_with("//") {
        if leading_whitespace != 0 {
            tokens.push(LosslessToken {
                kind: LosslessTokenKind::Whitespace,
                range: text_range(start, start + leading_whitespace)?,
            });
        }
        tokens.push(LosslessToken {
            kind: LosslessTokenKind::Comment,
            range: text_range(start + leading_whitespace, end)?,
        });
        return Ok(());
    }

    let mut run_start = 0;
    let mut run_kind = None;
    for (offset, character) in content.char_indices() {
        let kind = if matches!(character, ' ' | '\t') {
            LosslessTokenKind::Whitespace
        } else {
            LosslessTokenKind::Text
        };

        if run_kind.is_some_and(|current| current != kind) {
            tokens.push(LosslessToken {
                kind: run_kind.expect("a changed run has a previous kind"),
                range: text_range(start + run_start, start + offset)?,
            });
            run_start = offset;
        }
        run_kind = Some(kind);
    }

    if let Some(kind) = run_kind {
        tokens.push(LosslessToken {
            kind,
            range: text_range(start + run_start, end)?,
        });
    }
    Ok(())
}

fn text_range(start: usize, end: usize) -> Result<TextRange, PositionError> {
    TextRange::new(TextSize::new(start)?, TextSize::new(end)?)
}

#[cfg(test)]
mod tests {
    use super::{LineEnding, LosslessTokenKind, SourceLines};
    use crate::source::{TextRange, TextSize};

    #[test]
    fn source_lines_distinguish_empty_input_and_trailing_newline() {
        let empty = SourceLines::new("").expect("valid source");
        assert_eq!(empty.lines().len(), 1);
        assert_eq!(empty.text(empty.lines()[0].full_range()), Some(""));
        assert_eq!(empty.lines()[0].ending(), LineEnding::None);

        let terminated = SourceLines::new("text\n").expect("valid source");
        assert_eq!(terminated.lines().len(), 2);
        assert_eq!(
            terminated.text(terminated.lines()[0].content_range()),
            Some("text")
        );
        assert_eq!(
            terminated.text(terminated.lines()[0].ending_range()),
            Some("\n")
        );
        assert_eq!(terminated.lines()[0].ending(), LineEnding::Lf);
        assert_eq!(
            terminated.text(terminated.lines()[1].full_range()),
            Some("")
        );
    }

    #[test]
    fn source_lines_recognize_empty_lines_and_mixed_endings() {
        let source = "\n\r\nlast";
        let parsed = SourceLines::new(source).expect("valid source");

        assert_eq!(parsed.lines().len(), 3);
        assert_eq!(parsed.lines()[0].ending(), LineEnding::Lf);
        assert_eq!(parsed.lines()[1].ending(), LineEnding::CrLf);
        assert_eq!(parsed.lines()[2].ending(), LineEnding::None);
        assert_eq!(parsed.text(parsed.lines()[0].content_range()), Some(""));
        assert_eq!(parsed.text(parsed.lines()[1].content_range()), Some(""));
        assert_eq!(parsed.text(parsed.lines()[2].content_range()), Some("last"));
    }

    #[test]
    fn source_lines_keep_crlf_as_one_token() {
        let parsed = SourceLines::new("a\r\nb").expect("valid source");
        let ending = parsed
            .tokens()
            .iter()
            .find(|token| matches!(token.kind, LosslessTokenKind::LineEnding(_)))
            .expect("line ending token");

        assert_eq!(ending.kind, LosslessTokenKind::LineEnding(LineEnding::CrLf));
        assert_eq!(parsed.text(ending.range), Some("\r\n"));
    }

    #[test]
    fn source_lines_preserve_whitespace_comments_and_unicode() {
        let source = "\t// 日本語 😀\ntext  value";
        let parsed = SourceLines::new(source).expect("valid source");
        let kinds = parsed
            .tokens()
            .iter()
            .map(|token| token.kind)
            .collect::<Vec<_>>();

        assert_eq!(
            kinds,
            [
                LosslessTokenKind::Whitespace,
                LosslessTokenKind::Comment,
                LosslessTokenKind::LineEnding(LineEnding::Lf),
                LosslessTokenKind::Text,
                LosslessTokenKind::Whitespace,
                LosslessTokenKind::Text,
            ]
        );
        assert_eq!(parsed.reconstruct().as_bytes(), source.as_bytes());
    }

    #[test]
    fn source_lines_token_ranges_are_contiguous_and_lossless() {
        let sources = [
            "",
            "plain",
            "\n",
            "\r\n",
            "a\n\nb\r\n",
            "\u{feff}\0\ttext\n",
            " // comment\r\nnext",
        ];

        for source in sources {
            let parsed = SourceLines::new(source).expect("valid source");
            let mut expected_start = 0;
            for token in parsed.tokens() {
                assert_eq!(token.range.start().to_usize(), expected_start);
                expected_start = token.range.end().to_usize();
            }
            assert_eq!(expected_start, source.len());
            assert_eq!(parsed.reconstruct().as_bytes(), source.as_bytes());
        }
    }

    #[test]
    fn source_lines_reject_invalid_slice_boundaries_without_panicking() {
        let parsed = SourceLines::new("😀").expect("valid source");
        let invalid = TextRange::new(
            TextSize::new(1).expect("small offset"),
            TextSize::new(2).expect("small offset"),
        )
        .expect("ordered range");

        assert_eq!(parsed.text(invalid), None);
    }

    #[test]
    fn source_lines_accept_one_mib_single_line_boundary() {
        let source = "x".repeat(1024 * 1024);
        let parsed = SourceLines::new(&source).expect("valid source");

        assert_eq!(parsed.lines().len(), 1);
        assert_eq!(
            parsed.lines()[0].content_range().end().to_usize(),
            1024 * 1024
        );
        assert_eq!(parsed.reconstruct(), source);
    }
}
