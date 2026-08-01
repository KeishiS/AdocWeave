use std::collections::BTreeMap;

use super::DirectiveKind;
use crate::substitution::{AttributeExpansionLimits, expand_attribute_text};

#[derive(Clone, Debug)]
pub(super) struct ParsedDirective {
    pub(super) kind: DirectiveKind,
    pub(super) target: String,
    pub(super) attributes: String,
    pub(super) target_start: usize,
    pub(super) target_end: usize,
}

#[derive(Clone, Debug)]
pub(super) enum RecognizedDirective<'a> {
    Conditional(ParsedDirective),
    Include(ParsedDirective),
    Escaped(&'a str),
    Text,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ConditionalTransition {
    Inline { selected: bool },
    Open { enabled: bool },
    Close,
}

/// A preprocessor directive line, classified without evaluating it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DirectiveLine {
    Conditional,
    Include,
}

/// Classifies one line as a preprocessor directive.
///
/// The preprocessor consumes these lines before parsing, so the block grammar
/// never saw them and read `ifeval::["a" == "b"]` as an inline macro: the
/// leading `ifeval:` looked like a URL scheme, and the reader was told the URL
/// was rejected. An analysis that does not preprocess still has to recognize
/// the line, and both callers use this function so the lexical knowledge of a
/// directive stays in one place.
///
/// An escaped directive (`\ifdef::web[]`) is not a directive and returns
/// `None`, matching [`recognize`].
pub(crate) fn classify_line(value: &str) -> Option<DirectiveLine> {
    if parse_conditional(value).is_some() {
        return Some(DirectiveLine::Conditional);
    }
    parse_include(value).map(|_| DirectiveLine::Include)
}

pub(super) fn recognize(value: &str) -> RecognizedDirective<'_> {
    if let Some(directive) = parse_conditional(value) {
        return RecognizedDirective::Conditional(directive);
    }
    if let Some(directive) = parse_include(value) {
        return RecognizedDirective::Include(directive);
    }
    if let Some(literal) = value.strip_prefix('\\')
        && (parse_include(literal).is_some() || parse_conditional(literal).is_some())
    {
        return RecognizedDirective::Escaped(literal);
    }
    RecognizedDirective::Text
}

pub(super) fn transition(
    directive: &ParsedDirective,
    parent_enabled: bool,
    attributes: &BTreeMap<String, String>,
    limits: AttributeExpansionLimits,
) -> ConditionalTransition {
    match directive.kind {
        DirectiveKind::Ifdef | DirectiveKind::Ifndef if !directive.attributes.is_empty() => {
            ConditionalTransition::Inline {
                selected: parent_enabled
                    && attribute_condition(
                        &directive.target,
                        attributes,
                        directive.kind == DirectiveKind::Ifdef,
                    ),
            }
        }
        DirectiveKind::Ifdef => ConditionalTransition::Open {
            enabled: parent_enabled && attribute_condition(&directive.target, attributes, true),
        },
        DirectiveKind::Ifndef => ConditionalTransition::Open {
            enabled: parent_enabled && attribute_condition(&directive.target, attributes, false),
        },
        DirectiveKind::Ifeval => ConditionalTransition::Open {
            enabled: parent_enabled
                && evaluate_expression(&expand_attributes(
                    &directive.attributes,
                    attributes,
                    limits,
                )),
        },
        DirectiveKind::Endif => ConditionalTransition::Close,
        DirectiveKind::Include => unreachable!("include is not a conditional transition"),
    }
}

/// Substitutes attribute references in a directive's own text.
///
/// The values handed in are already fully expanded, so this performs one
/// substitution rather than the recursive evaluation the document body uses.
/// The two must still agree on what an attribute reference *is*: a document
/// that writes `\{name}` means the literal text in a conditional expression
/// exactly as it does in a paragraph.
pub(super) fn expand_attributes(
    value: &str,
    attributes: &BTreeMap<String, String>,
    limits: AttributeExpansionLimits,
) -> String {
    // What this shares with the document body is the reading of the text: which
    // braces open a reference, what `\{` means, and where a reference ends. Two
    // answers to that question would let the same document mean different
    // things in a paragraph and in a conditional expression.
    //
    // The values handed in were already expanded under the caller's limits, so
    // the resolver never recurses and reports depth zero. The size limit is
    // raised for the substitution itself: charging a value twice would reject a
    // directive whose attribute the caller already accepted.
    let limits = AttributeExpansionLimits {
        max_bytes: u32::MAX,
        ..limits
    };
    expand_attribute_text(value, limits, |name| {
        Ok((
            attributes
                .get(name)
                .cloned()
                // An attribute with no value is left as written, so a reader
                // sees the reference that did not resolve.
                .unwrap_or_else(|| format!("{{{name}}}")),
            0,
        ))
    })
    .map_or_else(|_| value.to_owned(), |(expanded, _)| expanded)
}

fn parse_include(value: &str) -> Option<ParsedDirective> {
    parse(value, "include::", DirectiveKind::Include)
}

fn parse_conditional(value: &str) -> Option<ParsedDirective> {
    [
        ("ifdef::", DirectiveKind::Ifdef),
        ("ifndef::", DirectiveKind::Ifndef),
        ("ifeval::", DirectiveKind::Ifeval),
        ("endif::", DirectiveKind::Endif),
    ]
    .into_iter()
    .find_map(|(prefix, kind)| parse(value, prefix, kind))
}

fn parse(value: &str, prefix: &str, kind: DirectiveKind) -> Option<ParsedDirective> {
    let rest = value.strip_prefix(prefix)?;
    let bracket = rest.find('[')?;
    let close = rest.rfind(']')?;
    (close == rest.len() - 1 && bracket <= close).then(|| ParsedDirective {
        kind,
        target: rest[..bracket].to_owned(),
        attributes: rest[bracket + 1..close].to_owned(),
        target_start: prefix.len(),
        target_end: prefix.len() + bracket,
    })
}

fn attribute_condition(target: &str, attributes: &BTreeMap<String, String>, present: bool) -> bool {
    let matches = if target.contains('+') {
        target
            .split('+')
            .all(|name| attributes.contains_key(name.trim()))
    } else {
        target
            .split(',')
            .any(|name| attributes.contains_key(name.trim()))
    };
    if present { matches } else { !matches }
}

fn evaluate_expression(value: &str) -> bool {
    for operator in ["==", "!=", ">=", "<=", ">", "<"] {
        if let Some((left, right)) = value.split_once(operator) {
            let left = left.trim().trim_matches(['\'', '"']);
            let right = right.trim().trim_matches(['\'', '"']);
            let numeric = left.parse::<f64>().ok().zip(right.parse::<f64>().ok());
            return match (operator, numeric) {
                ("==", _) => left == right,
                ("!=", _) => left != right,
                (">=", Some((left, right))) => left >= right,
                ("<=", Some((left, right))) => left <= right,
                (">", Some((left, right))) => left > right,
                ("<", Some((left, right))) => left < right,
                _ => false,
            };
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_limits() -> AttributeExpansionLimits {
        AttributeExpansionLimits {
            max_depth: 16,
            max_bytes: 1024,
        }
    }

    /// A directive reads attribute references exactly as the body does.
    ///
    /// The same document must not mean one thing in a paragraph and another in
    /// a conditional expression.
    #[test]
    fn directive_text_reads_attribute_references_like_the_document_body() {
        let attributes = BTreeMap::from([("name".to_owned(), "value".to_owned())]);
        let expand = |value: &str| expand_attributes(value, &attributes, test_limits());

        assert_eq!(expand("{name}"), "value");
        assert_eq!(expand("{missing}"), "{missing}");
        assert_eq!(expand("{}"), "{}");
        // An escaped brace is literal text, which is what the body does with it.
        assert_eq!(expand("\\{name}"), "{name}");
        assert_eq!(expand("a \\{name} b {name}"), "a {name} b value");
        // An unterminated reference is left as written.
        assert_eq!(expand("{name"), "{name");
    }

    #[test]
    fn recognition_distinguishes_complete_escaped_and_text_lines() {
        for (source, expected) in [
            ("ifdef::web[]", "conditional"),
            ("include::part.adoc[]", "include"),
            ("\\ifndef::print[]", "escaped"),
            ("ifdef::web[", "text"),
            ("ordinary text", "text"),
        ] {
            let actual = match recognize(source) {
                RecognizedDirective::Conditional(_) => "conditional",
                RecognizedDirective::Include(_) => "include",
                RecognizedDirective::Escaped(_) => "escaped",
                RecognizedDirective::Text => "text",
            };
            assert_eq!(actual, expected, "{source}");
        }
    }

    #[test]
    fn conditional_transition_table_covers_every_form_and_parent_state() {
        let attributes = BTreeMap::from([
            ("web".to_owned(), String::new()),
            ("count".to_owned(), "2".to_owned()),
        ]);
        for (source, parent_enabled, expected) in [
            (
                "ifdef::web[]",
                true,
                ConditionalTransition::Open { enabled: true },
            ),
            (
                "ifdef::missing[]",
                true,
                ConditionalTransition::Open { enabled: false },
            ),
            (
                "ifndef::missing[]",
                true,
                ConditionalTransition::Open { enabled: true },
            ),
            (
                "ifndef::web[]",
                true,
                ConditionalTransition::Open { enabled: false },
            ),
            (
                "ifeval::[\"{count}\" >= \"2\"]",
                true,
                ConditionalTransition::Open { enabled: true },
            ),
            (
                "ifdef::web[inline]",
                true,
                ConditionalTransition::Inline { selected: true },
            ),
            (
                "ifndef::web[inline]",
                true,
                ConditionalTransition::Inline { selected: false },
            ),
            (
                "ifdef::web[]",
                false,
                ConditionalTransition::Open { enabled: false },
            ),
            (
                "ifdef::web[inline]",
                false,
                ConditionalTransition::Inline { selected: false },
            ),
            ("endif::[]", true, ConditionalTransition::Close),
            ("endif::[]", false, ConditionalTransition::Close),
        ] {
            let RecognizedDirective::Conditional(directive) = recognize(source) else {
                panic!("expected conditional: {source}");
            };
            assert_eq!(
                transition(&directive, parent_enabled, &attributes, test_limits()),
                expected,
                "{source}, parent_enabled={parent_enabled}"
            );
        }
    }
}
