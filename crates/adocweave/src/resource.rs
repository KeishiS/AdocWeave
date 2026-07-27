//! Host boundary for media resources referenced by standard macros.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;

use crate::core::SourceId;
use crate::inline::{MacroAttribute, MacroForm, StandardMacro, StandardMacroKind};
use crate::source::TextRange;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ResourcePurpose {
    Image,
    Icon,
    Audio,
    Video,
    VideoPoster,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceReference {
    purpose: ResourcePurpose,
    form: MacroForm,
    owner_range: TextRange,
    range: TextRange,
    target_range: TextRange,
    target: String,
    attributes: Vec<MacroAttribute>,
}

impl ResourceReference {
    pub(crate) fn from_macro(node: &StandardMacro) -> Vec<Self> {
        let purpose = match node.kind {
            StandardMacroKind::Image => ResourcePurpose::Image,
            StandardMacroKind::Icon => ResourcePurpose::Icon,
            StandardMacroKind::Audio => ResourcePurpose::Audio,
            StandardMacroKind::Video => ResourcePurpose::Video,
            _ => return Vec::new(),
        };
        let mut references = vec![Self {
            purpose,
            form: node.form,
            owner_range: node.range,
            range: node.range,
            target_range: node.target_range,
            target: node.target.clone(),
            attributes: node.attributes.clone(),
        }];
        if node.kind == StandardMacroKind::Video
            && let Some(poster) = node
                .attributes
                .iter()
                .find(|attribute| attribute.name.as_deref() == Some("poster"))
                .filter(|attribute| !attribute.value.is_empty())
        {
            references.push(Self {
                purpose: ResourcePurpose::VideoPoster,
                form: node.form,
                owner_range: node.range,
                range: poster.value_range,
                target_range: poster.value_range,
                target: poster.value.clone(),
                attributes: Vec::new(),
            });
        }
        references
    }

    pub const fn purpose(&self) -> ResourcePurpose {
        self.purpose
    }

    pub const fn form(&self) -> MacroForm {
        self.form
    }

    pub const fn owner_range(&self) -> TextRange {
        self.owner_range
    }

    pub const fn range(&self) -> TextRange {
        self.range
    }

    pub const fn target_range(&self) -> TextRange {
        self.target_range
    }

    pub fn target(&self) -> &str {
        &self.target
    }

    pub fn attributes(&self) -> &[MacroAttribute] {
        &self.attributes
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceQuery {
    pub source_id: Option<SourceId>,
    pub reference: ResourceReference,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceValue {
    pub href: String,
    pub media_type: MediaType,
    pub byte_length: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum MediaFamily {
    Image,
    Audio,
    Video,
    Other,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MediaType {
    essence: String,
    family: MediaFamily,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidMediaType;

impl MediaType {
    pub fn parse(value: &str) -> Result<Self, InvalidMediaType> {
        if !value.is_ascii() || value.bytes().any(|byte| byte.is_ascii_control()) {
            return Err(InvalidMediaType);
        }
        let mut parts = value.split(';');
        let essence = parts.next().ok_or(InvalidMediaType)?.trim();
        let (top_level, subtype) = essence.split_once('/').ok_or(InvalidMediaType)?;
        if top_level.is_empty()
            || subtype.is_empty()
            || top_level == "*"
            || subtype == "*"
            || subtype.contains('/')
            || !top_level.bytes().all(is_media_type_token)
            || !subtype.bytes().all(is_media_type_token)
        {
            return Err(InvalidMediaType);
        }
        if !parts.all(valid_media_type_parameter) {
            return Err(InvalidMediaType);
        }
        let family = if top_level.eq_ignore_ascii_case("image") {
            MediaFamily::Image
        } else if top_level.eq_ignore_ascii_case("audio") {
            MediaFamily::Audio
        } else if top_level.eq_ignore_ascii_case("video") {
            MediaFamily::Video
        } else {
            MediaFamily::Other
        };
        Ok(Self {
            essence: essence.to_ascii_lowercase(),
            family,
        })
    }

    pub fn essence(&self) -> &str {
        &self.essence
    }

    pub const fn family(&self) -> MediaFamily {
        self.family
    }
}

impl FromStr for MediaType {
    type Err = InvalidMediaType;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl fmt::Display for InvalidMediaType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid media type")
    }
}

impl std::error::Error for InvalidMediaType {}

fn is_media_type_token(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

fn valid_media_type_parameter(parameter: &str) -> bool {
    let parameter = parameter.trim();
    let Some((name, value)) = parameter.split_once('=') else {
        return false;
    };
    let name = name.trim();
    let value = value.trim();
    !name.is_empty()
        && name.bytes().all(is_media_type_token)
        && (value.bytes().all(is_media_type_token) || valid_quoted_parameter(value))
}

fn valid_quoted_parameter(value: &str) -> bool {
    let Some(inner) = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    else {
        return false;
    };
    let mut escaped = false;
    for byte in inner.bytes() {
        if escaped {
            escaped = false;
        } else if byte == b'\\' {
            escaped = true;
        } else if byte == b'"' {
            return false;
        }
    }
    !escaped
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceFailureKind {
    Missing,
    OutsideRoot,
    SchemeDenied,
    PermissionDenied,
    MediaTypeUnavailable,
    ResolverFailure,
}

impl ResourceFailureKind {
    pub const fn diagnostic_code(self) -> &'static str {
        match self {
            Self::Missing => "missing-resource",
            Self::OutsideRoot => "resource-outside-root",
            Self::SchemeDenied => "resource-scheme-denied",
            Self::PermissionDenied => "resource-permission-denied",
            Self::MediaTypeUnavailable => "resource-media-type-unavailable",
            Self::ResolverFailure => "resource-resolver-failure",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceFailure {
    pub kind: ResourceFailureKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedResource {
    pub source_range: TextRange,
    pub outcome: ResourceOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResourceOutcome {
    Resolved(ResourceValue),
    Failed(ResourceFailure),
}

impl ResolvedResource {
    pub fn resolved(
        source_range: TextRange,
        href: impl Into<String>,
        media_type: MediaType,
        byte_length: Option<u64>,
    ) -> Self {
        Self {
            source_range,
            outcome: ResourceOutcome::Resolved(ResourceValue {
                href: href.into(),
                media_type,
                byte_length,
            }),
        }
    }

    pub fn failed(source_range: TextRange, failure: ResourceFailure) -> Self {
        Self {
            source_range,
            outcome: ResourceOutcome::Failed(failure),
        }
    }
}

pub type ResourceFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ResourceValue, ResourceFailure>> + Send + 'a>>;

/// Resource I/O is exclusively owned by the host and is never called while parsing.
pub trait ResourceResolver: Send + Sync {
    fn resolve<'a>(&'a self, query: &'a ResourceQuery) -> ResourceFuture<'a>;
}

#[cfg(test)]
mod tests {
    use super::{MediaFamily, MediaType, ResourcePurpose};
    use crate::{Engine, ParseOptions};

    #[test]
    fn media_type_requires_a_concrete_ascii_essence() {
        let image = MediaType::parse("IMAGE/PNG; charset=binary").expect("media type");
        assert_eq!(image.essence(), "image/png");
        assert_eq!(image.family(), MediaFamily::Image);
        assert_eq!(
            MediaType::parse("application/octet-stream")
                .expect("media type")
                .family(),
            MediaFamily::Other
        );
        for invalid in [
            "image/",
            "image/*",
            "image/png;",
            "image/png; =",
            "image/png; arbitrary garbage",
            "image/png; charset=\"unterminated",
            "image/png\nvideo/mp4",
            "image/png;\r\ninvalid",
            "画像/png",
        ] {
            assert!(MediaType::parse(invalid).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn poster_query_uses_the_unquoted_utf8_value_range() {
        let source = "video:demo.mp4[Demo, poster = \"ポスター.jpg\"]";
        let analysis = Engine::new(ParseOptions::default())
            .analyze(source)
            .expect("analysis");
        let poster = analysis
            .resources()
            .iter()
            .find(|reference| reference.purpose() == ResourcePurpose::VideoPoster)
            .expect("poster");
        assert_eq!(poster.target(), "ポスター.jpg");
        assert_eq!(
            analysis.source_document().text(poster.target_range()),
            Some("ポスター.jpg")
        );
        assert_eq!(poster.range(), poster.target_range());
    }

    #[test]
    fn empty_or_shadowed_poster_does_not_create_an_ambiguous_query() {
        let empty = Engine::new(ParseOptions::default())
            .analyze("video:demo.mp4[poster=]")
            .expect("analysis");
        assert_eq!(empty.resources().len(), 1);

        let duplicate = Engine::new(ParseOptions::default())
            .analyze("video:demo.mp4[poster=first.jpg,poster=second.jpg]")
            .expect("analysis");
        let posters = duplicate
            .resources()
            .iter()
            .filter(|reference| reference.purpose() == ResourcePurpose::VideoPoster)
            .collect::<Vec<_>>();
        assert_eq!(posters.len(), 1);
        assert_eq!(posters[0].target(), "first.jpg");
    }
}
