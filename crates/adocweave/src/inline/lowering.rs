//! Semantic construction from recognized inline token ranges.

use crate::budget::{BudgetExceeded, ParseBudget};
use crate::inline_model::{
    AttributeUse, Inline, InlineLiteralKind, InlineProblem, InlineProblemKind, InlineStyle,
    InlineText, MacroAttribute, Reference, ReferenceDestination, StandardMacro,
};
use crate::source::TextRange;

use super::{
    BuiltInline, InlineParseConfig, MarkerToken, ReferenceToken, StandardMacroToken, parse_segment,
    subrange, valid_attribute_name,
};

pub(super) fn lower_marker(
    value: &str,
    range: TextRange,
    config: InlineParseConfig,
    depth: usize,
    token: MarkerToken,
    budget: &mut ParseBudget,
) -> Result<BuiltInline, BudgetExceeded> {
    let MarkerToken {
        open,
        close,
        end,
        marker,
        form,
    } = token;
    let marker_width = form.width();
    let node_range = subrange(range, open, end);
    let content_range = subrange(range, open + marker_width, close);
    let mut problems = Vec::new();
    let inline = match marker {
        '`' => Inline::Literal {
            kind: InlineLiteralKind::Monospace,
            range: node_range,
            content_range,
            value: value[open + marker_width..close].to_owned(),
        },
        '*' | '_' | '#' | '~' | '^' if depth >= config.max_depth => {
            problems.push(InlineProblem {
                kind: InlineProblemKind::NestingLimitExceeded,
                range: node_range,
            });
            Inline::Text(InlineText {
                range: node_range,
                value: value[open..end].to_owned(),
            })
        }
        '*' | '_' | '#' | '~' | '^' => {
            let inner = parse_segment(
                &value[open + marker_width..close],
                content_range,
                config,
                depth + 1,
                budget,
            )?;
            problems.extend(inner.problems);
            Inline::Styled {
                style: match marker {
                    '*' => InlineStyle::Strong,
                    '_' => InlineStyle::Emphasis,
                    '#' => InlineStyle::Highlight,
                    '~' => InlineStyle::Subscript,
                    '^' => InlineStyle::Superscript,
                    _ => unreachable!(),
                },
                range: node_range,
                content_range,
                children: inner.inlines,
            }
        }
        '{' => Inline::AttributeReference {
            range: node_range,
            name_range: content_range,
            name: value[open + marker_width..close].to_owned(),
            value: None,
            expansion_error: None,
        },
        _ => unreachable!("only supported markers are returned"),
    };
    Ok(BuiltInline {
        inline,
        end,
        problems,
    })
}

pub(super) fn lower_standard_macro(
    value: &str,
    range: TextRange,
    token: StandardMacroToken,
) -> BuiltInline {
    let attributes_range = subrange(range, token.bracket + 1, token.close);
    BuiltInline {
        inline: Inline::Macro(StandardMacro {
            kind: token.kind,
            form: token.form,
            range: subrange(range, token.open, token.end),
            target_range: subrange(range, token.target_start, token.bracket),
            target_source: value[token.target_start..token.bracket].to_owned(),
            target: value[token.target_start..token.bracket].to_owned(),
            target_attributes: attribute_uses(
                &value[token.target_start..token.bracket],
                subrange(range, token.target_start, token.bracket),
            ),
            target_expansion_error: None,
            attributes_range,
            attributes: parse_macro_attributes(
                &value[token.bracket + 1..token.close],
                attributes_range,
            ),
        }),
        end: token.end,
        problems: Vec::new(),
    }
}

fn parse_macro_attributes(value: &str, range: TextRange) -> Vec<MacroAttribute> {
    let mut attributes = Vec::new();
    let mut start = 0;
    let mut quote = None;
    for (offset, character) in value
        .char_indices()
        .chain(std::iter::once((value.len(), ',')))
    {
        if matches!(character, '\'' | '"') {
            quote = if quote == Some(character) {
                None
            } else if quote.is_none() {
                Some(character)
            } else {
                quote
            };
        }
        if character != ',' || quote.is_some() {
            continue;
        }
        let raw = &value[start..offset];
        let leading = raw.len() - raw.trim_start().len();
        let trailing = raw.len() - raw.trim_end().len();
        let item_start = start + leading;
        let item_end = offset.saturating_sub(trailing);
        if item_start < item_end {
            let item = &value[item_start..item_end];
            let (name, raw_value, raw_value_start) =
                item.find('=').map_or((None, item, item_start), |equals| {
                    (
                        Some(item[..equals].trim().to_owned()),
                        &item[equals + 1..],
                        item_start + equals + 1,
                    )
                });
            let value_leading = raw_value.len() - raw_value.trim_start().len();
            let value_trailing = raw_value.len() - raw_value.trim_end().len();
            let mut value_start = raw_value_start + value_leading;
            let mut value_end = item_end.saturating_sub(value_trailing);
            let mut item_value = &value[value_start..value_end];
            if item_value.len() >= 2 {
                let first = item_value.as_bytes()[0];
                let last = item_value.as_bytes()[item_value.len() - 1];
                if matches!(first, b'\'' | b'"') && first == last {
                    value_start += 1;
                    value_end -= 1;
                    item_value = &value[value_start..value_end];
                }
            }
            attributes.push(MacroAttribute {
                range: subrange(range, item_start, item_end),
                value_range: subrange(range, value_start, value_end),
                name,
                value: item_value.to_owned(),
            });
        }
        start = offset + 1;
    }
    attributes
}

pub(super) fn lower_reference(
    value: &str,
    range: TextRange,
    config: InlineParseConfig,
    depth: usize,
    token: ReferenceToken,
    budget: &mut ParseBudget,
) -> Result<BuiltInline, BudgetExceeded> {
    budget.consume_reference()?;
    match token {
        ReferenceToken::Short {
            open,
            target_start,
            close,
            end,
        } => {
            let target = &value[target_start..close];
            let (anchor, label) = target
                .split_once(',')
                .map_or((target, None), |(anchor, label)| (anchor, Some(label)));
            let target_range = subrange(range, target_start, target_start + anchor.len());
            let label_range = label.map(|label| subrange(range, close - label.len(), close));
            let label_output = label.map(|label| {
                parse_segment(
                    label,
                    label_range.expect("label has range"),
                    config,
                    depth + 1,
                    budget,
                )
            });
            let label_output = label_output.transpose()?;
            let (label_inlines, problems) = label_output.map_or_else(
                || (Vec::new(), Vec::new()),
                |output| (output.inlines, output.problems),
            );
            Ok(BuiltInline {
                inline: Inline::Reference(Reference {
                    range: subrange(range, open, end),
                    macro_name_range: None,
                    target_range,
                    target_source: anchor.to_owned(),
                    expanded_target: anchor.to_owned(),
                    target_attributes: attribute_uses(anchor, target_range),
                    target_expansion_error: None,
                    authored_destination: if anchor.is_empty() {
                        ReferenceDestination::Invalid
                    } else {
                        ReferenceDestination::Local {
                            anchor: anchor.to_owned(),
                            anchor_range: target_range,
                        }
                    },
                    target: (!anchor.is_empty()).then(|| crate::reference::ReferenceKey::Local {
                        anchor: anchor.to_owned(),
                    }),
                    label_range,
                    label: label_inlines,
                }),
                end,
                problems,
            })
        }
        ReferenceToken::Xref {
            open,
            target_start,
            bracket,
            close,
            end,
        } => {
            let target = &value[target_start..bracket];
            let label_text = &value[bracket + 1..close];
            let target_range = subrange(range, target_start, bracket);
            let label_range = subrange(range, bracket + 1, close);
            let label = parse_segment(label_text, label_range, config, depth + 1, budget)?;
            Ok(BuiltInline {
                inline: Inline::Reference(Reference {
                    range: subrange(range, open, end),
                    macro_name_range: Some(subrange(range, open, target_start - 1)),
                    target_range,
                    target_source: target.to_owned(),
                    expanded_target: target.to_owned(),
                    target_attributes: attribute_uses(target, target_range),
                    target_expansion_error: None,
                    authored_destination: parse_reference_destination(target, target_range),
                    target: crate::reference::ReferenceKey::parse(target),
                    label_range: Some(label_range),
                    label: label.inlines,
                }),
                end,
                problems: label.problems,
            })
        }
    }
}

fn parse_reference_destination(target: &str, range: TextRange) -> ReferenceDestination {
    if let Some(anchor) = target.strip_prefix('#') {
        return if anchor.is_empty() {
            ReferenceDestination::Invalid
        } else {
            ReferenceDestination::Local {
                anchor: anchor.to_owned(),
                anchor_range: subrange(range, 1, target.len()),
            }
        };
    }
    if let Some(colon) = target.find(':') {
        let scheme = &target[..colon];
        if scheme.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphanumeric() || (index > 0 && matches!(byte, b'+' | b'-' | b'.'))
        }) {
            let remainder = &target[colon + 1..];
            let (locator, anchor) = remainder
                .split_once('#')
                .map_or((remainder, None), |(locator, anchor)| {
                    (locator, Some(anchor))
                });
            let locator_start = colon + 1;
            return ReferenceDestination::Scheme {
                scheme: scheme.to_ascii_lowercase(),
                scheme_range: subrange(range, 0, colon),
                locator: locator.to_owned(),
                locator_range: subrange(range, locator_start, locator_start + locator.len()),
                anchor: anchor.map(str::to_owned),
                anchor_range: anchor
                    .map(|anchor| subrange(range, target.len() - anchor.len(), target.len())),
            };
        }
    }
    let (document, anchor) = target
        .split_once('#')
        .map_or((target, None), |(document, anchor)| {
            (document, Some(anchor))
        });
    if document.is_empty() {
        ReferenceDestination::Invalid
    } else {
        ReferenceDestination::Document {
            document: document.to_owned(),
            document_range: subrange(range, 0, document.len()),
            anchor: anchor.map(str::to_owned),
            anchor_range: anchor
                .map(|anchor| subrange(range, target.len() - anchor.len(), target.len())),
        }
    }
}

pub(super) fn attribute_uses(value: &str, range: TextRange) -> Vec<AttributeUse> {
    let mut output = Vec::new();
    let mut cursor = 0;
    while let Some(open_relative) = value[cursor..].find('{') {
        let open = cursor + open_relative;
        let Some(close_relative) = value[open + 1..].find('}') else {
            break;
        };
        let close = open + 1 + close_relative;
        let name = &value[open + 1..close];
        if valid_attribute_name(name) {
            output.push(AttributeUse {
                name: name.to_owned(),
                name_range: subrange(range, open + 1, close),
            });
        }
        cursor = close + 1;
    }
    output
}
