use super::super::safe::{
    ActiveUrlAttributeName, AttributeValue, ElementName, HtmlWriter, PassiveAttributeName,
};
use super::plan::{DocumentHeadPlan, PlannedStylesheet};

/// Serializes an already validated head plan. This function performs no policy
/// decisions and receives no unclassified stylesheet strings or URLs.
pub(in crate::html) fn serialize_document_head(plan: &DocumentHeadPlan<'_>) -> String {
    let mut output = String::new();
    let mut writer = HtmlWriter::new(&mut output);

    writer.start(element("head"));
    writer.finish_start();
    writer.line_break();

    writer.start(element("meta"));
    writer.passive_attribute(passive_attribute("charset"), AttributeValue::new("utf-8"));
    writer.finish_start();
    writer.line_break();

    writer.start(element("title"));
    writer.finish_start();
    writer.text(plan.title);
    writer.end(element("title"));
    writer.line_break();

    for stylesheet in &plan.stylesheets {
        match *stylesheet {
            PlannedStylesheet::Inline(css) => {
                writer.start(element("style"));
                writer.finish_start();
                writer.line_break();
                writer.safe_style_body(css);
                if !css.ends_with_line_break() {
                    writer.line_break();
                }
                writer.end(element("style"));
                writer.line_break();
            }
            PlannedStylesheet::External(url) => {
                writer.start(element("link"));
                writer
                    .passive_attribute(passive_attribute("rel"), AttributeValue::new("stylesheet"));
                writer.active_url_attribute(active_url_attribute("href"), url);
                writer.finish_start();
                writer.line_break();
            }
        }
    }

    writer.end(element("head"));
    writer.line_break();
    output
}

fn element(name: &'static str) -> ElementName<'static> {
    ElementName::new(name).expect("document head uses allowlisted elements")
}

fn passive_attribute(name: &'static str) -> PassiveAttributeName<'static> {
    PassiveAttributeName::new(name).expect("document head uses allowlisted passive attributes")
}

fn active_url_attribute(name: &'static str) -> ActiveUrlAttributeName<'static> {
    ActiveUrlAttributeName::new(name).expect("document head uses active URL attributes")
}

#[cfg(test)]
mod tests {
    use crate::html::safe::{SafeStyleBody, SafeUrl, TextValue};
    use crate::url::{ActiveUrlPolicy, UrlProvenance};

    use super::*;

    #[test]
    fn serializer_has_deterministic_order_and_escaping() {
        let policy = ActiveUrlPolicy::default();
        let plan = DocumentHeadPlan {
            title: TextValue::new("<Title>"),
            stylesheets: vec![
                PlannedStylesheet::External(
                    SafeUrl::from_policy(
                        "https://example.com/a.css?a=1&b=2",
                        &policy,
                        UrlProvenance::ResolvedResource,
                    )
                    .expect("safe URL"),
                ),
                PlannedStylesheet::Inline(
                    SafeStyleBody::new("p { margin: 0; }").expect("safe CSS"),
                ),
            ],
        };

        assert_eq!(
            serialize_document_head(&plan),
            concat!(
                "<head>\n",
                "<meta charset=\"utf-8\">\n",
                "<title>&lt;Title&gt;</title>\n",
                "<link rel=\"stylesheet\" href=\"https://example.com/a.css?a=1&amp;b=2\">\n",
                "<style>\n",
                "p { margin: 0; }\n",
                "</style>\n",
                "</head>\n"
            )
        );
    }
}
