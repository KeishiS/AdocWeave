//! URL policies for source diagnostics and active output.

use std::collections::BTreeSet;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthoredUrlPolicy {
    pub allowed_schemes: BTreeSet<String>,
    pub allow_relative: bool,
}

impl Default for AuthoredUrlPolicy {
    fn default() -> Self {
        Self {
            allowed_schemes: default_allowed_schemes(),
            allow_relative: true,
        }
    }
}

impl AuthoredUrlPolicy {
    pub fn allows(&self, value: &str) -> bool {
        self.classify(value) == UrlDecision::Allowed
    }

    pub fn classify(&self, value: &str) -> UrlDecision {
        if invalid_url_text(value) || contains_incomplete_scheme(value, &self.allowed_schemes) {
            return UrlDecision::Rejected;
        }
        let Some(colon) = value.find(':') else {
            return classify_authored_relative(value, self.allow_relative);
        };
        classify_scheme(value, colon, &self.allowed_schemes, false)
    }
}

/// Security policy applied immediately before a URL becomes active output.
///
/// Resolver output is untrusted and is checked separately from URLs authored
/// in the source document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActiveUrlPolicy {
    pub allowed_schemes: BTreeSet<String>,
    pub allow_authored_relative: bool,
    pub allow_resolved_relative: bool,
    pub allow_resolved_root_relative: bool,
    pub allow_data_uris: bool,
}

impl Default for ActiveUrlPolicy {
    fn default() -> Self {
        Self {
            allowed_schemes: default_allowed_schemes(),
            allow_authored_relative: false,
            allow_resolved_relative: false,
            allow_resolved_root_relative: false,
            allow_data_uris: false,
        }
    }
}

impl ActiveUrlPolicy {
    pub fn allows(&self, value: &str, provenance: UrlProvenance) -> bool {
        self.classify(value, provenance) == UrlDecision::Allowed
    }

    pub fn classify(&self, value: &str, provenance: UrlProvenance) -> UrlDecision {
        let allow_relative = match provenance {
            UrlProvenance::Authored => self.allow_authored_relative,
            UrlProvenance::ResolvedReference | UrlProvenance::ResolvedResource => {
                self.allow_resolved_relative
            }
        };
        classify_url(
            value,
            &self.allowed_schemes,
            allow_relative,
            provenance.is_resolved() && self.allow_resolved_root_relative,
            self.allow_data_uris,
        )
    }
}

fn default_allowed_schemes() -> BTreeSet<String> {
    ["http", "https"].map(String::from).into_iter().collect()
}

/// Schemes that never become active output, whatever a host allows.
///
/// `allowed_schemes` is filled in by the host, and a host that adds one of
/// these has asked for a URL the browser executes as code. The stylesheet
/// policy already refuses to emit such a URL regardless of configuration; this
/// list gives the same guarantee to every other active URL. `data` stays out of
/// it because a data URI carries inert content and has its own switch.
const NEVER_ACTIVE_SCHEMES: &[&str] = &["javascript", "vbscript"];

fn classify_url(
    value: &str,
    allowed_schemes: &BTreeSet<String>,
    allow_relative: bool,
    allow_root_relative: bool,
    allow_data_uris: bool,
) -> UrlDecision {
    if invalid_url_text(value) || contains_incomplete_scheme(value, allowed_schemes) {
        return UrlDecision::Rejected;
    }
    let Some(colon) = value.find(':') else {
        return classify_relative(value, allow_relative, allow_root_relative);
    };
    classify_scheme(value, colon, allowed_schemes, allow_data_uris)
}

fn classify_scheme(
    value: &str,
    colon: usize,
    allowed_schemes: &BTreeSet<String>,
    allow_data_uris: bool,
) -> UrlDecision {
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
    if NEVER_ACTIVE_SCHEMES.contains(&normalized.as_str()) {
        return UrlDecision::Rejected;
    }
    if normalized == "data" && !allow_data_uris {
        return UrlDecision::Rejected;
    }
    if allowed_schemes
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(&normalized))
    {
        UrlDecision::Allowed
    } else {
        UrlDecision::Rejected
    }
}

fn invalid_url_text(value: &str) -> bool {
    value.is_empty()
        || value.chars().any(|character| {
            character.is_control()
                || character.is_whitespace()
                || matches!(character, '<' | '>' | '"' | '\'' | '`' | '{' | '}')
        })
        || contains_invalid_percent_escape(value)
        || contains_encoded_control(value)
}

fn contains_invalid_percent_escape(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            index += 1;
            continue;
        }
        if index + 2 >= bytes.len()
            || hex(bytes[index + 1]).is_none()
            || hex(bytes[index + 2]).is_none()
        {
            return true;
        }
        index += 3;
    }
    false
}

fn contains_incomplete_scheme(value: &str, allowed_schemes: &BTreeSet<String>) -> bool {
    let Some(separator) = value.find("//") else {
        return false;
    };
    let prefix = &value[..separator];
    !prefix.is_empty()
        && prefix.as_bytes()[0].is_ascii_alphabetic()
        && prefix.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphanumeric() || (index > 0 && matches!(byte, b'+' | b'-' | b'.'))
        })
        && allowed_schemes
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(prefix))
}

/// Judges a relative path a document author wrote.
///
/// This answers "is this worth a diagnostic", not "is this safe to emit". A
/// document may legitimately link to `../guide.adoc`, and an encoded segment
/// may be the correct spelling of a real filename, so neither is reported here.
/// [`classify_relative`] refuses both, because it decides what becomes active
/// output and a resolver has no reason to produce either.
///
/// A backslash and a root-relative path are refused in both places: neither is
/// a relative path an author meant to write.
fn classify_authored_relative(value: &str, allow_relative: bool) -> UrlDecision {
    if !allow_relative || value.starts_with('/') || value.contains('\\') {
        return UrlDecision::Rejected;
    }
    UrlDecision::Allowed
}

/// Judges a relative path immediately before it becomes active output.
fn classify_relative(value: &str, allow_relative: bool, allow_root_relative: bool) -> UrlDecision {
    if value.contains('\\')
        || value.split('/').any(|segment| segment == "..")
        || contains_encoded_path_metacharacter(value)
    {
        return UrlDecision::Rejected;
    }
    if value.starts_with('/') {
        return if allow_root_relative && !value.starts_with("//") {
            UrlDecision::Allowed
        } else {
            UrlDecision::Rejected
        };
    }
    if allow_relative {
        UrlDecision::Allowed
    } else {
        UrlDecision::Rejected
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
pub enum UrlProvenance {
    Authored,
    ResolvedReference,
    ResolvedResource,
}

impl UrlProvenance {
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
    use std::collections::BTreeSet;

    use super::{ActiveUrlPolicy, AuthoredUrlPolicy, UrlDecision, UrlProvenance};

    /// A host cannot allow a scheme the browser executes as code.
    #[test]
    fn schemes_that_execute_are_refused_whatever_the_host_allows() {
        let hostile: BTreeSet<String> = ["javascript", "vbscript", "JavaScript"]
            .into_iter()
            .map(String::from)
            .collect();
        let active = ActiveUrlPolicy {
            allowed_schemes: hostile.clone(),
            allow_data_uris: true,
            ..ActiveUrlPolicy::default()
        };
        let authored = AuthoredUrlPolicy {
            allowed_schemes: hostile,
            allow_relative: true,
        };

        for value in [
            "javascript:alert(1)",
            "JavaScript:alert(1)",
            "vbscript:msgbox(1)",
        ] {
            assert_eq!(
                active.classify(value, UrlProvenance::Authored),
                UrlDecision::Rejected,
                "{value}"
            );
            assert_eq!(authored.classify(value), UrlDecision::Rejected, "{value}");
        }

        // A data URI carries inert content and keeps its own switch.
        assert_eq!(
            ActiveUrlPolicy {
                allowed_schemes: ["data"].map(String::from).into_iter().collect(),
                allow_data_uris: true,
                ..ActiveUrlPolicy::default()
            }
            .classify("data:image/png;base64,AA", UrlProvenance::ResolvedResource),
            UrlDecision::Allowed
        );
    }

    #[test]
    fn authored_and_active_policies_are_independent() {
        let authored = AuthoredUrlPolicy::default();
        let active = ActiveUrlPolicy::default();

        assert_eq!(authored.classify("guide.adoc"), UrlDecision::Allowed);
        assert_eq!(authored.classify("../guide.adoc"), UrlDecision::Allowed);
        assert_eq!(
            active.classify("guide.adoc", UrlProvenance::Authored),
            UrlDecision::Rejected
        );
    }

    #[test]
    fn root_relative_urls_are_allowed_only_for_resolver_output() {
        let policy = ActiveUrlPolicy {
            allow_resolved_root_relative: true,
            ..ActiveUrlPolicy::default()
        };

        assert_eq!(
            policy.classify("/notes/123", UrlProvenance::Authored),
            UrlDecision::Rejected
        );
        assert_eq!(
            policy.classify("/notes/123", UrlProvenance::ResolvedReference),
            UrlDecision::Allowed
        );
        assert_eq!(
            policy.classify("/assets/image.png", UrlProvenance::ResolvedResource),
            UrlDecision::Allowed
        );
        assert_eq!(
            policy.classify("//evil.example/path", UrlProvenance::ResolvedReference),
            UrlDecision::Rejected
        );
        assert_eq!(
            policy.classify("/../secret", UrlProvenance::ResolvedReference),
            UrlDecision::Rejected
        );
        assert_eq!(
            policy.classify("/%2e%2e/secret", UrlProvenance::ResolvedReference),
            UrlDecision::Rejected
        );
    }

    #[test]
    fn malformed_url_syntax_is_rejected_by_both_policies() {
        let authored = AuthoredUrlPolicy::default();
        let active = ActiveUrlPolicy {
            allow_authored_relative: true,
            ..ActiveUrlPolicy::default()
        };

        for value in ["http//example.com", "bad%ZZpath", "trailing%", "short%0"] {
            assert_eq!(authored.classify(value), UrlDecision::Rejected, "{value}");
            assert_eq!(
                active.classify(value, UrlProvenance::Authored),
                UrlDecision::Rejected,
                "{value}"
            );
        }
    }

    #[test]
    fn relative_double_slashes_and_case_insensitive_configuration_are_supported() {
        let authored = AuthoredUrlPolicy {
            allowed_schemes: ["HTTPS".to_owned()].into_iter().collect(),
            ..AuthoredUrlPolicy::default()
        };

        assert_eq!(authored.classify("images//logo.png"), UrlDecision::Allowed);
        assert_eq!(
            authored.classify("https://example.com/logo.png"),
            UrlDecision::Allowed
        );
        assert_eq!(
            authored.classify("https//example.com/logo.png"),
            UrlDecision::Rejected
        );
    }
}
