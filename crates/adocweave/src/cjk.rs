//! Recognition of characters written without spaces between words.
//!
//! Chinese, Japanese and Korean text separates words by character shape rather
//! than by spaces. Two rules in this crate depend on knowing whether a character
//! belongs to such a script, and both must agree: the inline macro boundary in
//! [`crate::inline`] and the paragraph line join in [`crate::html`]. Keeping the
//! test here is what stops one of them from drifting away from the other.

/// Returns whether a character belongs to a script written without word spaces.
///
/// The ranges cover the scripts themselves and the punctuation that is written
/// with them. Punctuation matters as much as the letters do: a Japanese sentence
/// ends with `。` and encloses text in `「」`, and text on either side of those
/// marks is no more in need of a space than text between two ideographs.
///
/// Latin letters and digits are deliberately absent. A boundary between a
/// Latin word and a CJK word is where a space genuinely belongs.
pub(crate) const fn is_cjk(character: char) -> bool {
    matches!(character,
        // CJK Symbols and Punctuation, Hiragana, Katakana, and the ideographic
        // space. `。`, `、`, `「`, `」`, `・` and the kana all live here.
        '\u{3000}'..='\u{30FF}'
        // CJK Unified Ideographs Extension A.
        | '\u{3400}'..='\u{4DBF}'
        // CJK Unified Ideographs.
        | '\u{4E00}'..='\u{9FFF}'
        // Hangul Syllables and the Jamo that compose them.
        | '\u{1100}'..='\u{11FF}'
        | '\u{AC00}'..='\u{D7AF}'
        // CJK Compatibility Ideographs.
        | '\u{F900}'..='\u{FAFF}'
        // Halfwidth and Fullwidth Forms. `（`, `）` and fullwidth Latin belong
        // to text set on a CJK grid, so they follow the same spacing.
        | '\u{FF00}'..='\u{FFEF}'
        // CJK Unified Ideographs Extension B through F.
        | '\u{20000}'..='\u{2FA1F}'
    )
}

/// Returns whether two characters read as one run without a space between them.
///
/// Both sides must be present and belong to a script written without word
/// spaces. A boundary with Latin text, a digit, or the edge of the text keeps
/// its space: that is where a space carries meaning.
pub(crate) fn joins_without_space(before: Option<char>, after: Option<char>) -> bool {
    matches!((before, after), (Some(before), Some(after)) if is_cjk(before) && is_cjk(after))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_written_without_word_spaces_are_recognized() {
        for character in [
            '日', '本', '語', 'あ', 'ア', '。', '、', '「', '」', '（', '）', '한',
        ] {
            assert!(is_cjk(character), "{character}");
        }
    }

    #[test]
    fn characters_that_separate_words_with_spaces_are_not_recognized() {
        for character in ['a', 'Z', '0', ' ', '.', ',', '(', ')', '-', '\n', 'é', 'Я'] {
            assert!(!is_cjk(character), "{character:?}");
        }
    }
}
