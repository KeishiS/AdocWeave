//! Output-neutral URL security policy shared by lint and renderers.

use std::collections::BTreeSet;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UrlPolicy {
    pub allowed_schemes: BTreeSet<String>,
    pub allow_relative: bool,
    pub allow_data_uris: bool,
}

impl Default for UrlPolicy {
    fn default() -> Self {
        Self {
            allowed_schemes: ["http", "https"].map(String::from).into_iter().collect(),
            allow_relative: false,
            allow_data_uris: false,
        }
    }
}

impl UrlPolicy {
    pub fn allows(&self, value: &str) -> bool {
        self.classify(value) == UrlDecision::Allowed
    }

    pub fn classify(&self, value: &str) -> UrlDecision {
        if value.is_empty()
            || value.chars().any(|character| {
                character.is_control()
                    || character.is_whitespace()
                    || matches!(character, '<' | '>' | '"' | '\'' | '`' | '{' | '}')
            })
            || contains_encoded_control(value)
        {
            return UrlDecision::Rejected;
        }
        let Some(colon) = value.find(':') else {
            return if self.allow_relative
                && !value.starts_with('/')
                && !value.starts_with('\\')
                && !value.contains('\\')
                && !value.split('/').any(|segment| segment == "..")
            {
                UrlDecision::Allowed
            } else {
                UrlDecision::Rejected
            };
        };
        let scheme = &value[..colon];
        if scheme.is_empty()
            || !scheme.bytes().enumerate().all(|(index, byte)| {
                byte.is_ascii_alphanumeric() || (index > 0 && matches!(byte, b'+' | b'-' | b'.'))
            })
            || !scheme.as_bytes()[0].is_ascii_alphabetic()
        {
            return UrlDecision::Rejected;
        }
        let normalized = scheme.to_ascii_lowercase();
        if normalized == "data" && !self.allow_data_uris {
            return UrlDecision::Rejected;
        }
        if self.allowed_schemes.contains(&normalized) {
            UrlDecision::Allowed
        } else {
            UrlDecision::Rejected
        }
    }
}

fn contains_encoded_control(value: &str) -> bool {
    value.as_bytes().windows(3).any(|window| {
        if window[0] != b'%' {
            return false;
        }
        let (Some(high), Some(low)) = (hex(window[1]), hex(window[2])) else {
            return false;
        };
        let decoded = high * 16 + low;
        decoded <= 0x20 || decoded == 0x7f
    })
}

const fn hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UrlDecision {
    Allowed,
    Rejected,
}
