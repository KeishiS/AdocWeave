//! Typed, I/O-free discovery of authored local-file candidates.

use crate::inline_model::{Link, Reference, ReferenceDestination};
use crate::reference::ReferenceKey;
use crate::resource::{ResourcePurpose, ResourceReference};
use crate::source::TextRange;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum LocalTargetKind {
    Link,
    CrossReference,
    Resource(ResourcePurpose),
    Include,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalTargetSyntax {
    Candidate,
    Unverifiable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalTargetReference {
    pub kind: LocalTargetKind,
    pub range: TextRange,
    pub target_range: TextRange,
    pub target: String,
    pub path: String,
    pub syntax: LocalTargetSyntax,
}

impl LocalTargetReference {
    pub fn from_link(link: &Link) -> Option<Self> {
        from_target(
            LocalTargetKind::Link,
            link.range,
            link.target_range,
            &link.target,
            link.target_expansion_error.is_some(),
            true,
        )
    }

    pub fn from_reference(reference: &Reference) -> Option<Self> {
        let Some(ReferenceKey::Document { document, .. }) = &reference.target else {
            return None;
        };
        let target_range = match &reference.authored_destination {
            ReferenceDestination::Document { document_range, .. } => *document_range,
            _ => reference.target_range,
        };
        from_target(
            LocalTargetKind::CrossReference,
            reference.range,
            target_range,
            document,
            reference.target_expansion_error.is_some(),
            true,
        )
    }

    pub fn from_resource(reference: &ResourceReference) -> Option<Self> {
        from_target(
            LocalTargetKind::Resource(reference.purpose()),
            reference.range(),
            reference.target_range(),
            reference.target(),
            reference.target_expansion_error().is_some(),
            true,
        )
    }

    pub fn from_include(range: TextRange, target_range: TextRange, target: &str) -> Option<Self> {
        from_target(
            LocalTargetKind::Include,
            range,
            target_range,
            target,
            target.contains(['{', '}']),
            false,
        )
    }
}

fn from_target(
    kind: LocalTargetKind,
    range: TextRange,
    target_range: TextRange,
    target: &str,
    expansion_failed: bool,
    strip_url_suffix: bool,
) -> Option<LocalTargetReference> {
    let path = if strip_url_suffix {
        target
            .split_once(['?', '#'])
            .map_or(target, |(path, _)| path)
    } else {
        target
    };
    if expansion_failed {
        return Some(LocalTargetReference {
            kind,
            range,
            target_range,
            target: target.to_owned(),
            path: path.to_owned(),
            syntax: LocalTargetSyntax::Unverifiable,
        });
    }
    if path.is_empty() {
        return None;
    }
    if path.starts_with(['/', '\\']) || path.contains(':') {
        return None;
    }
    let rejected_relative = crate::url::AuthoredUrlPolicy::default().classify(target)
        == crate::url::UrlDecision::Rejected;
    let syntax = if rejected_relative
        || path.contains('\\')
        || path.chars().any(|character| {
            character.is_control()
                || character.is_whitespace()
                || matches!(character, '<' | '>' | '"' | '\'' | '`' | '{' | '}')
        })
        || invalid_percent_escape(path)
        || encoded_forbidden_byte(path)
    {
        LocalTargetSyntax::Unverifiable
    } else {
        LocalTargetSyntax::Candidate
    };
    Some(LocalTargetReference {
        kind,
        range,
        target_range,
        target: target.to_owned(),
        path: path.to_owned(),
        syntax,
    })
}

fn invalid_percent_escape(value: &str) -> bool {
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

fn encoded_forbidden_byte(value: &str) -> bool {
    value.as_bytes().windows(3).any(|window| {
        if window[0] != b'%' {
            return false;
        }
        let (Some(high), Some(low)) = (hex(window[1]), hex(window[2])) else {
            return false;
        };
        matches!(high * 16 + low, 0..=31 | 58 | 92 | 127)
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

#[cfg(test)]
mod tests {
    use super::{LocalTargetKind, LocalTargetReference, LocalTargetSyntax};
    use crate::source::{TextRange, TextSize};

    fn range() -> TextRange {
        TextRange::new(TextSize::new(0).unwrap(), TextSize::new(1).unwrap()).unwrap()
    }

    #[test]
    fn include_candidates_distinguish_local_nonlocal_and_unverifiable_targets() {
        let local =
            LocalTargetReference::from_include(range(), range(), "../guide.adoc?view=1#top")
                .expect("local");
        assert_eq!(local.kind, LocalTargetKind::Include);
        assert_eq!(local.path, "../guide.adoc?view=1#top");
        assert_eq!(local.syntax, LocalTargetSyntax::Candidate);

        assert!(
            LocalTargetReference::from_include(range(), range(), "https://example.com").is_none()
        );
        assert_eq!(
            LocalTargetReference::from_include(range(), range(), "#local")
                .expect("literal local filename")
                .path,
            "#local"
        );
        assert_eq!(
            LocalTargetReference::from_include(range(), range(), "{missing}.adoc")
                .expect("unverifiable")
                .syntax,
            LocalTargetSyntax::Unverifiable
        );
        for target in ["bad%0Aname", "bad%7Fname", "stream%3Adata", "bad%5Cname"] {
            assert_eq!(
                LocalTargetReference::from_include(range(), range(), target)
                    .expect("local syntax")
                    .syntax,
                LocalTargetSyntax::Unverifiable
            );
        }
    }
}
