//! Minimal deterministic JSON primitives shared by stable text contracts.

use std::fmt::Write as _;

pub(crate) fn write_string(output: &mut String, value: &str) {
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            '\u{08}' => output.push_str("\\b"),
            '\u{0c}' => output.push_str("\\f"),
            '\u{00}'..='\u{1f}' => {
                write!(output, "\\u{:04x}", u32::from(character))
                    .expect("writing to String cannot fail");
            }
            _ => output.push(character),
        }
    }
    output.push('"');
}

pub(crate) fn string(value: &str) -> String {
    let mut output = String::new();
    write_string(&mut output, value);
    output
}

#[cfg(test)]
mod tests {
    #[test]
    fn escapes_every_json_control_character_class() {
        assert_eq!(
            super::string("\"\\\u{08}\u{0c}\n\r\t\u{01}"),
            "\"\\\"\\\\\\b\\f\\n\\r\\t\\u0001\""
        );
    }
}
