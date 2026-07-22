//! Output-neutral URL security policy shared by lint and renderers.

use std::collections::BTreeSet;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UrlPolicy {
    pub allowed_schemes: BTreeSet<String>,
    /// Allows non-root relative URLs authored in the document.
    pub allow_relative: bool,
    /// Allows non-root relative URLs returned by a host resolver.
    pub allow_resolved_relative: bool,
    /// Allows single-slash root-relative URLs returned by a host resolver.
    pub allow_resolved_root_relative: bool,
    pub allow_data_uris: bool,
}

impl Default for UrlPolicy {
    fn default() -> Self {
        Self {
            allowed_schemes: ["http", "https"].map(String::from).into_iter().collect(),
            allow_relative: false,
            allow_resolved_relative: false,
            allow_resolved_root_relative: false,
            allow_data_uris: false,
        }
    }
}

impl UrlPolicy {
    pub fn allows(&self, value: &str, context: UrlContext) -> bool {
        self.classify(value, context) == UrlDecision::Allowed
    }

    pub fn classify(&self, value: &str, context: UrlContext) -> UrlDecision {
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
            return self.classify_relative(value, context);
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

    fn classify_relative(&self, value: &str, context: UrlContext) -> UrlDecision {
        if value.contains('\\')
            || value.split('/').any(|segment| segment == "..")
            || contains_encoded_path_metacharacter(value)
        {
            return UrlDecision::Rejected;
        }
        if value.starts_with('/') {
            return if context.is_resolved()
                && self.allow_resolved_root_relative
                && !value.starts_with("//")
            {
                UrlDecision::Allowed
            } else {
                UrlDecision::Rejected
            };
        }
        let allowed = match context {
            UrlContext::AuthoredLink => self.allow_relative,
            UrlContext::ResolvedReference | UrlContext::ResolvedResource => {
                self.allow_resolved_relative
            }
        };
        if allowed {
            UrlDecision::Allowed
        } else {
            UrlDecision::Rejected
        }
    }
}

fn contains_encoded_path_metacharacter(value: &str) -> bool {
    value.as_bytes().windows(3).any(|window| {
        if window[0] != b'%' {
            return false;
        }
        let (Some(high), Some(low)) = (hex(window[1]), hex(window[2])) else {
            return false;
        };
        matches!(high * 16 + low, b'.' | b'/' | b'\\')
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UrlContext {
    AuthoredLink,
    ResolvedReference,
    ResolvedResource,
}

impl UrlContext {
    const fn is_resolved(self) -> bool {
        matches!(self, Self::ResolvedReference | Self::ResolvedResource)
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

#[cfg(test)]
mod tests {
    use super::{UrlContext, UrlDecision, UrlPolicy};

    #[test]
    fn root_relative_urls_are_allowed_only_for_resolver_contexts() {
        let policy = UrlPolicy {
            allow_resolved_root_relative: true,
            ..UrlPolicy::default()
        };

        assert_eq!(
            policy.classify("/notes/123", UrlContext::AuthoredLink),
            UrlDecision::Rejected
        );
        assert_eq!(
            policy.classify("/notes/123", UrlContext::ResolvedReference),
            UrlDecision::Allowed
        );
        assert_eq!(
            policy.classify("/assets/image.png", UrlContext::ResolvedResource),
            UrlDecision::Allowed
        );
        assert_eq!(
            policy.classify("//evil.example/path", UrlContext::ResolvedReference),
            UrlDecision::Rejected
        );
        assert_eq!(
            policy.classify("/../secret", UrlContext::ResolvedReference),
            UrlDecision::Rejected
        );
        assert_eq!(
            policy.classify("/%2e%2e/secret", UrlContext::ResolvedReference),
            UrlDecision::Rejected
        );
    }
}
