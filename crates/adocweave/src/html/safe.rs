use super::{ALLOWED_ATTRIBUTES, ALLOWED_CLASSES, ALLOWED_ELEMENTS};
use crate::url::{ActiveUrlPolicy, UrlDecision, UrlProvenance};

const ACTIVE_URL_ATTRIBUTES: &[&str] = &["href", "poster", "src"];
const BOOLEAN_ATTRIBUTES: &[&str] = &["controls"];
const CLASS_ATTRIBUTE: &str = "class";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ElementName<'a>(&'a str);

impl<'a> ElementName<'a> {
    pub(super) fn new(value: &'a str) -> Option<Self> {
        ALLOWED_ELEMENTS.contains(&value).then_some(Self(value))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PassiveAttributeName<'a>(&'a str);

impl<'a> PassiveAttributeName<'a> {
    pub(super) fn new(value: &'a str) -> Option<Self> {
        (ALLOWED_ATTRIBUTES.contains(&value)
            && !ACTIVE_URL_ATTRIBUTES.contains(&value)
            && !BOOLEAN_ATTRIBUTES.contains(&value)
            && value != CLASS_ATTRIBUTE)
            .then_some(Self(value))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct BooleanAttributeName<'a>(&'a str);

impl<'a> BooleanAttributeName<'a> {
    pub(super) fn new(value: &'a str) -> Option<Self> {
        BOOLEAN_ATTRIBUTES.contains(&value).then_some(Self(value))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ActiveUrlAttributeName<'a>(&'a str);

impl<'a> ActiveUrlAttributeName<'a> {
    pub(super) fn new(value: &'a str) -> Option<Self> {
        ACTIVE_URL_ATTRIBUTES
            .contains(&value)
            .then_some(Self(value))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ClassName<'a>(&'a str);

impl<'a> ClassName<'a> {
    pub(super) fn new(value: &'a str) -> Option<Self> {
        (value != "language-*" && ALLOWED_CLASSES.contains(&value)).then_some(Self(value))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SourceLanguageClass(String);

impl SourceLanguageClass {
    pub(super) fn new(language: &str) -> Option<Self> {
        let language = crate::projection::canonical_source_language(language);
        (!language.is_empty()).then(|| Self(format!("language-{language}")))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct TextValue<'a>(&'a str);

impl<'a> TextValue<'a> {
    pub(super) const fn new(value: &'a str) -> Self {
        Self(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct AttributeValue<'a>(&'a str);

impl<'a> AttributeValue<'a> {
    pub(super) const fn new(value: &'a str) -> Self {
        Self(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SafeUrl<'a>(&'a str);

impl<'a> SafeUrl<'a> {
    pub(super) fn from_policy(
        value: &'a str,
        policy: &ActiveUrlPolicy,
        provenance: UrlProvenance,
    ) -> Option<Self> {
        (policy.classify(value, provenance) == UrlDecision::Allowed).then_some(Self(value))
    }

    pub(super) fn into_owned(self) -> OwnedSafeUrl {
        OwnedSafeUrl(self.0.to_owned())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct OwnedSafeUrl(String);

impl OwnedSafeUrl {
    pub(super) fn from_policy(
        value: String,
        policy: &ActiveUrlPolicy,
        provenance: UrlProvenance,
    ) -> Option<Self> {
        (policy.classify(&value, provenance) == UrlDecision::Allowed).then_some(Self(value))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SafeFragmentUrl<'a>(&'a str);

impl<'a> SafeFragmentUrl<'a> {
    pub(super) fn new(anchor: &'a str) -> Option<Self> {
        (!anchor.is_empty() && !anchor.chars().any(char::is_control)).then_some(Self(anchor))
    }

    pub(super) fn into_owned(self) -> OwnedSafeFragmentUrl {
        OwnedSafeFragmentUrl(self.0.to_owned())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct OwnedSafeFragmentUrl(String);

/// Host-supplied CSS that cannot terminate its `<style>` element or open an
/// HTML comment context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SafeStyleBody<'a>(&'a str);

impl<'a> SafeStyleBody<'a> {
    pub(super) fn new(value: &'a str) -> Option<Self> {
        if value
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
            || value.contains("<!--")
        {
            return None;
        }
        let closes_style = value
            .as_bytes()
            .windows("</style".len())
            .any(|window| window.eq_ignore_ascii_case(b"</style"));
        (!closes_style).then_some(Self(value))
    }

    pub(super) fn ends_with_line_break(self) -> bool {
        self.0.as_bytes().last() == Some(&b'\n')
    }
}

pub(super) struct HtmlWriter<'a> {
    output: &'a mut String,
}

impl<'a> HtmlWriter<'a> {
    pub(super) const fn new(output: &'a mut String) -> Self {
        Self { output }
    }

    pub(super) fn start(&mut self, element: ElementName<'_>) {
        self.output.push('<');
        self.output.push_str(element.0);
    }

    pub(super) fn passive_attribute(
        &mut self,
        name: PassiveAttributeName<'_>,
        value: AttributeValue<'_>,
    ) {
        self.attribute(name.0, value.0);
    }

    pub(super) fn active_url_attribute(
        &mut self,
        name: ActiveUrlAttributeName<'_>,
        value: SafeUrl<'_>,
    ) {
        self.attribute(name.0, value.0);
    }

    pub(super) fn owned_active_url_attribute(
        &mut self,
        name: ActiveUrlAttributeName<'_>,
        value: &OwnedSafeUrl,
    ) {
        self.attribute(name.0, &value.0);
    }

    pub(super) fn owned_fragment_url_attribute(
        &mut self,
        name: ActiveUrlAttributeName<'_>,
        value: &OwnedSafeFragmentUrl,
    ) {
        self.output.push(' ');
        self.output.push_str(name.0);
        self.output.push_str("=\"#");
        escape_into(self.output, &value.0);
        self.output.push('"');
    }

    pub(super) fn class_attribute(&mut self, classes: &[ClassName<'_>]) {
        self.output.push_str(" class=\"");
        for (index, class) in classes.iter().enumerate() {
            if index > 0 {
                self.output.push(' ');
            }
            escape_into(self.output, class.0);
        }
        self.output.push('"');
    }

    pub(super) fn source_language_class_attribute(&mut self, class: &SourceLanguageClass) {
        self.attribute(CLASS_ATTRIBUTE, &class.0);
    }

    pub(super) fn boolean_attribute(&mut self, name: BooleanAttributeName<'_>) {
        self.output.push(' ');
        self.output.push_str(name.0);
    }

    pub(super) fn finish_start(&mut self) {
        self.output.push('>');
    }

    pub(super) fn text(&mut self, value: TextValue<'_>) {
        escape_into(self.output, value.0);
    }

    pub(super) fn safe_style_body(&mut self, value: SafeStyleBody<'_>) {
        self.output.push_str(value.0);
    }

    pub(super) fn line_break(&mut self) {
        self.output.push('\n');
    }

    pub(super) fn inline_text(&mut self, value: TextValue<'_>) {
        let mut characters = value.0.chars().peekable();
        while let Some(character) = characters.next() {
            if character == '\r' {
                if characters.peek() == Some(&'\n') {
                    characters.next();
                }
                self.output.push(' ');
            } else if character == '\n' {
                self.output.push(' ');
            } else {
                let mut encoded = [0; 4];
                escape_into(self.output, character.encode_utf8(&mut encoded));
            }
        }
    }

    pub(super) fn end(&mut self, element: ElementName<'_>) {
        self.output.push_str("</");
        self.output.push_str(element.0);
        self.output.push('>');
    }

    fn attribute(&mut self, name: &str, value: &str) {
        self.output.push(' ');
        self.output.push_str(name);
        self.output.push_str("=\"");
        escape_into(self.output, value);
        self.output.push('"');
    }
}

fn escape_into(output: &mut String, text: &str) {
    for character in text.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&#34;"),
            '\'' => output.push_str("&#39;"),
            _ => output.push(character),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_fail_closed_outside_the_element_attribute_and_class_allowlists() {
        assert!(ElementName::new("p").is_some());
        assert!(ElementName::new("script").is_none());
        assert!(PassiveAttributeName::new("id").is_some());
        assert!(PassiveAttributeName::new("onclick").is_none());
        assert!(PassiveAttributeName::new("href").is_none());
        assert!(PassiveAttributeName::new("class").is_none());
        assert!(PassiveAttributeName::new("controls").is_none());
        assert!(ActiveUrlAttributeName::new("href").is_some());
        assert!(ActiveUrlAttributeName::new("id").is_none());
        assert!(BooleanAttributeName::new("controls").is_some());
        assert!(BooleanAttributeName::new("id").is_none());
        assert!(ClassName::new("admonition-note").is_some());
        assert!(ClassName::new("language-*").is_none());
        assert!(ClassName::new("attacker-controlled").is_none());
    }

    #[test]
    fn source_languages_use_a_dedicated_normalized_class_domain() {
        let class = SourceLanguageClass::new("Rust<&").expect("nonempty source language");
        let mut output = String::new();
        let mut writer = HtmlWriter::new(&mut output);
        writer.start(ElementName::new("code").expect("allowlisted element"));
        writer.source_language_class_attribute(&class);
        writer.finish_start();
        assert_eq!(output, "<code class=\"language-rust--\">");
        assert!(SourceLanguageClass::new("").is_none());
    }

    #[test]
    fn active_url_values_require_policy_acceptance_before_serialization() {
        let policy = ActiveUrlPolicy {
            allow_authored_relative: true,
            ..ActiveUrlPolicy::default()
        };
        assert!(
            SafeUrl::from_policy("javascript:alert(1)", &policy, UrlProvenance::Authored).is_none()
        );
        assert!(
            SafeUrl::from_policy(
                "https://example.com/\"bad",
                &policy,
                UrlProvenance::Authored
            )
            .is_none()
        );
        let safe = SafeUrl::from_policy(
            "https://example.com/?a=1&b=2",
            &policy,
            UrlProvenance::Authored,
        )
        .expect("safe URL");
        let mut output = String::new();
        let mut writer = HtmlWriter::new(&mut output);
        writer.start(ElementName::new("a").expect("allowlisted element"));
        writer.active_url_attribute(
            ActiveUrlAttributeName::new("href").expect("active URL attribute"),
            safe,
        );
        writer.finish_start();
        writer.text(TextValue::new("<label>"));
        writer.end(ElementName::new("a").expect("allowlisted element"));
        assert_eq!(
            output,
            "<a href=\"https://example.com/?a=1&amp;b=2\">&lt;label&gt;</a>"
        );
    }

    #[test]
    fn fragment_urls_require_nonempty_control_free_identifiers() {
        assert!(SafeFragmentUrl::new("").is_none());
        assert!(SafeFragmentUrl::new("unsafe\nanchor").is_none());
        let anchor = SafeFragmentUrl::new("section-日本語&more").expect("safe fragment");
        let mut output = String::new();
        let mut writer = HtmlWriter::new(&mut output);
        writer.start(ElementName::new("a").expect("allowlisted element"));
        writer.owned_fragment_url_attribute(
            ActiveUrlAttributeName::new("href").expect("active URL attribute"),
            &anchor.into_owned(),
        );
        writer.finish_start();
        assert_eq!(output, "<a href=\"#section-日本語&amp;more\">");
    }

    #[test]
    fn passive_attributes_classes_and_inline_text_have_fixed_escaping() {
        let mut output = String::new();
        let mut writer = HtmlWriter::new(&mut output);
        writer.start(ElementName::new("p").expect("allowlisted element"));
        writer.passive_attribute(
            PassiveAttributeName::new("id").expect("passive attribute"),
            AttributeValue::new("\"<&"),
        );
        writer.class_attribute(&[
            ClassName::new("admonition").expect("class"),
            ClassName::new("admonition-note").expect("class"),
        ]);
        writer.finish_start();
        writer.inline_text(TextValue::new("a\r\nb\n<&"));
        writer.end(ElementName::new("p").expect("allowlisted element"));
        assert_eq!(
            output,
            "<p id=\"&#34;&lt;&amp;\" class=\"admonition admonition-note\">a b &lt;&amp;</p>"
        );
    }

    #[test]
    fn style_bodies_cannot_escape_the_style_element() {
        assert!(SafeStyleBody::new("p { margin: 0; }\n").is_some());
        for unsafe_css in ["</style>", "</STYLE >", "<!--", "p {}\u{0}"] {
            assert!(SafeStyleBody::new(unsafe_css).is_none(), "{unsafe_css:?}");
        }
    }
}
