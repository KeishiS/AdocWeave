use std::collections::BTreeMap;

use super::DirectiveKind;

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
                && evaluate_expression(&expand_attributes(&directive.attributes, attributes)),
        },
        DirectiveKind::Endif => ConditionalTransition::Close,
        DirectiveKind::Include => unreachable!("include is not a conditional transition"),
    }
}

pub(super) fn expand_attributes(value: &str, attributes: &BTreeMap<String, String>) -> String {
    let mut output = String::new();
    let mut cursor = 0;
    while let Some(open) = value[cursor..].find('{').map(|offset| cursor + offset) {
        output.push_str(&value[cursor..open]);
        let Some(close) = value[open + 1..].find('}').map(|offset| open + 1 + offset) else {
            output.push_str(&value[open..]);
            return output;
        };
        let name = &value[open + 1..close];
        if let Some(replacement) = attributes.get(name) {
            output.push_str(replacement);
        } else {
            output.push_str(&value[open..=close]);
        }
        cursor = close + 1;
    }
    output.push_str(&value[cursor..]);
    output
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
                transition(&directive, parent_enabled, &attributes),
                expected,
                "{source}, parent_enabled={parent_enabled}"
            );
        }
    }
}
