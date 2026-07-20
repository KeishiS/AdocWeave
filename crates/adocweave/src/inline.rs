//! Output-independent inline syntax.

use crate::source::{TextRange, TextSize};

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct InlineText {
    pub range: TextRange,
    pub value: String,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct Link {
    pub range: TextRange,
    pub target_range: TextRange,
    pub target_source: String,
    pub target: String,
    pub target_attributes: Vec<AttributeUse>,
    pub label_range: Option<TextRange>,
    pub label: Vec<Inline>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct AttributeUse {
    pub name: String,
    pub name_range: TextRange,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub enum MathLanguage {
    Latex,
    Typst,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct InlineFormula {
    pub range: TextRange,
    pub content_range: TextRange,
    pub language: MathLanguage,
    pub value: String,
    pub closed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct Reference {
    pub range: TextRange,
    pub target_range: TextRange,
    pub target_source: String,
    pub destination: ReferenceDestination,
    pub label_range: Option<TextRange>,
    pub label: Vec<Inline>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub enum ReferenceDestination {
    Local {
        anchor: String,
        anchor_range: TextRange,
    },
    Document {
        document: String,
        document_range: TextRange,
        anchor: Option<String>,
        anchor_range: Option<TextRange>,
    },
    Scheme {
        scheme: String,
        scheme_range: TextRange,
        locator: String,
        locator_range: TextRange,
        anchor: Option<String>,
        anchor_range: Option<TextRange>,
    },
    Invalid,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub enum Inline {
    Text(InlineText),
    Literal {
        kind: InlineLiteralKind,
        range: TextRange,
        content_range: TextRange,
        value: String,
    },
    Styled {
        style: InlineStyle,
        range: TextRange,
        content_range: TextRange,
        children: Vec<Inline>,
    },
    AttributeReference {
        range: TextRange,
        name_range: TextRange,
        name: String,
    },
    Link(Link),
    Reference(Reference),
    Formula(InlineFormula),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub enum InlineLiteralKind {
    Monospace,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub enum InlineStyle {
    Strong,
    Emphasis,
}

impl Inline {
    pub const fn range(&self) -> TextRange {
        match self {
            Self::Text(text) => text.range,
            Self::Literal { range, .. }
            | Self::Styled { range, .. }
            | Self::AttributeReference { range, .. } => *range,
            Self::Link(link) => link.range,
            Self::Reference(reference) => reference.range,
            Self::Formula(formula) => formula.range,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub enum InlineProblemKind {
    UnclosedMonospace,
    UnclosedStrong,
    UnclosedEmphasis,
    NestingLimitExceeded,
    UnclosedAttributeReference,
    IncompleteLink,
    IncompleteCrossReference,
    InvalidCrossReference,
    UnclosedStem,
    EmptyStem,
    StemSizeLimitExceeded,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub struct InlineProblem {
    pub kind: InlineProblemKind,
    pub range: TextRange,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct InlineParseOutput {
    pub inlines: Vec<Inline>,
    pub problems: Vec<InlineProblem>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct InlineParseConfig {
    pub max_depth: usize,
    pub max_formula_bytes: usize,
}

impl Default for InlineParseConfig {
    fn default() -> Self {
        Self {
            max_depth: 32,
            max_formula_bytes: 1024 * 1024,
        }
    }
}

#[cfg(test)]
fn parse_text(value: &str, range: TextRange, config: InlineParseConfig) -> Vec<Inline> {
    parse(value, range, config).inlines
}

pub(crate) fn parse(value: &str, range: TextRange, config: InlineParseConfig) -> InlineParseOutput {
    parse_segment(value, range, config, 0)
}

fn parse_segment(
    value: &str,
    range: TextRange,
    config: InlineParseConfig,
    depth: usize,
) -> InlineParseOutput {
    let mut output = InlineParseOutput::default();
    let mut cursor = 0;
    let mut plain_start = 0;

    while let Some(candidate) = next_candidate(value, cursor) {
        match candidate {
            InlineCandidate::Macro { open } => {
                if let Some(built) = parse_macro(value, range, config, depth, open) {
                    if is_escaped(value, open) {
                        push_text(&mut output.inlines, value, range, plain_start, open - 1);
                        output.inlines.push(Inline::Text(InlineText {
                            range: subrange(range, open - 1, built.end),
                            value: value[open..built.end].to_owned(),
                        }));
                    } else {
                        push_text(&mut output.inlines, value, range, plain_start, open);
                        output.inlines.push(built.inline);
                        output.problems.extend(built.problems);
                    }
                    cursor = built.end;
                    plain_start = built.end;
                    continue;
                }
                cursor = next_char_boundary(value, open);
                continue;
            }
            InlineCandidate::Marker { open, marker } => {
                match recognize_marker(value, open, marker) {
                    MarkerRecognition::Complete(token) => {
                        let built = build_marker(value, range, config, depth, token);
                        push_text(&mut output.inlines, value, range, plain_start, open);
                        output.inlines.push(built.inline);
                        output.problems.extend(built.problems);
                        cursor = token.end;
                        plain_start = cursor;
                    }
                    MarkerRecognition::Unclosed { next, kind } => {
                        output.problems.push(InlineProblem {
                            kind,
                            range: subrange(range, open, next),
                        });
                        cursor = next;
                    }
                    MarkerRecognition::Invalid { next } => cursor = next,
                }
            }
        }
    }

    push_text(&mut output.inlines, value, range, plain_start, value.len());
    scan_incomplete_macros(value, range, &mut output.problems);
    output
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InlineCandidate {
    Macro { open: usize },
    Marker { open: usize, marker: char },
}

fn next_candidate(value: &str, cursor: usize) -> Option<InlineCandidate> {
    let marker = next_opener(value, cursor);
    let macro_open = next_macro_start(value, cursor);
    match (macro_open, marker) {
        (Some(open), Some((marker_open, _))) if open <= marker_open => {
            Some(InlineCandidate::Macro { open })
        }
        (Some(open), None) => Some(InlineCandidate::Macro { open }),
        (_, Some((open, marker))) => Some(InlineCandidate::Marker { open, marker }),
        (None, None) => None,
    }
}

fn next_char_boundary(value: &str, offset: usize) -> usize {
    offset + value[offset..].chars().next().map_or(1, char::len_utf8)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MarkerToken {
    open: usize,
    close: usize,
    end: usize,
    marker: char,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MarkerRecognition {
    Complete(MarkerToken),
    Unclosed {
        next: usize,
        kind: InlineProblemKind,
    },
    Invalid {
        next: usize,
    },
}

struct BuiltInline {
    inline: Inline,
    end: usize,
    problems: Vec<InlineProblem>,
}

fn recognize_marker(value: &str, open: usize, marker: char) -> MarkerRecognition {
    let next = open + marker.len_utf8();
    let Some(close) = find_closer(value, open, marker) else {
        let kind = match marker {
            '`' => InlineProblemKind::UnclosedMonospace,
            '*' => InlineProblemKind::UnclosedStrong,
            '_' => InlineProblemKind::UnclosedEmphasis,
            '{' => InlineProblemKind::UnclosedAttributeReference,
            _ => unreachable!("only supported markers are returned"),
        };
        return MarkerRecognition::Unclosed { next, kind };
    };
    if marker == '{' && !valid_attribute_name(&value[open + 1..close]) {
        return MarkerRecognition::Invalid { next };
    }
    MarkerRecognition::Complete(MarkerToken {
        open,
        close,
        end: close + marker.len_utf8(),
        marker,
    })
}

fn build_marker(
    value: &str,
    range: TextRange,
    config: InlineParseConfig,
    depth: usize,
    token: MarkerToken,
) -> BuiltInline {
    let MarkerToken {
        open,
        close,
        end,
        marker,
    } = token;
    let node_range = subrange(range, open, end);
    let content_range = subrange(range, open + marker.len_utf8(), close);
    let mut problems = Vec::new();
    let inline = match marker {
        '`' => Inline::Literal {
            kind: InlineLiteralKind::Monospace,
            range: node_range,
            content_range,
            value: value[open + 1..close].to_owned(),
        },
        '*' | '_' if depth >= config.max_depth => {
            problems.push(InlineProblem {
                kind: InlineProblemKind::NestingLimitExceeded,
                range: node_range,
            });
            Inline::Text(InlineText {
                range: node_range,
                value: value[open..end].to_owned(),
            })
        }
        '*' | '_' => {
            let inner = parse_segment(&value[open + 1..close], content_range, config, depth + 1);
            problems.extend(inner.problems);
            Inline::Styled {
                style: if marker == '*' {
                    InlineStyle::Strong
                } else {
                    InlineStyle::Emphasis
                },
                range: node_range,
                content_range,
                children: inner.inlines,
            }
        }
        '{' => Inline::AttributeReference {
            range: node_range,
            name_range: content_range,
            name: value[open + 1..close].to_owned(),
        },
        _ => unreachable!("only supported markers are returned"),
    };
    BuiltInline {
        inline,
        end,
        problems,
    }
}

fn valid_attribute_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn next_macro_start(value: &str, cursor: usize) -> Option<usize> {
    value[cursor..]
        .char_indices()
        .map(|(offset, _)| cursor + offset)
        .find(|offset| {
            let rest = &value[*offset..];
            if rest.starts_with("<<") || starts_ascii_case_insensitive(rest, "xref:") {
                return true;
            }
            if starts_ascii_case_insensitive(rest, "stem:[")
                || starts_ascii_case_insensitive(rest, "latexmath:[")
            {
                return true;
            }
            (is_token_boundary(value[..*offset].chars().next_back())
                || (is_escaped(value, *offset)
                    && is_token_boundary(value[..offset.saturating_sub(1)].chars().next_back())))
                && url_scheme_end(rest).is_some()
        })
}

fn parse_macro(
    value: &str,
    range: TextRange,
    config: InlineParseConfig,
    depth: usize,
    open: usize,
) -> Option<BuiltInline> {
    let token = recognize_macro(value, open)?;
    Some(build_macro(value, range, config, depth, token))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MacroToken {
    Formula(FormulaToken),
    Reference(ReferenceToken),
    Link(LinkToken),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FormulaToken {
    open: usize,
    content_start: usize,
    content_end: usize,
    end: usize,
    closed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReferenceToken {
    Short {
        open: usize,
        target_start: usize,
        close: usize,
        end: usize,
    },
    Xref {
        open: usize,
        target_start: usize,
        bracket: usize,
        close: usize,
        end: usize,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LinkToken {
    Explicit {
        open: usize,
        target_start: usize,
        bracket: usize,
        close: usize,
        end: usize,
    },
    Url {
        open: usize,
        target_end: usize,
        label: Option<(usize, usize)>,
        end: usize,
    },
}

fn recognize_macro(value: &str, open: usize) -> Option<MacroToken> {
    let rest = &value[open..];
    let formula_prefix = if starts_ascii_case_insensitive(rest, "stem:[") {
        Some("stem:[".len())
    } else if starts_ascii_case_insensitive(rest, "latexmath:[") {
        Some("latexmath:[".len())
    } else {
        None
    };
    if let Some(prefix_len) = formula_prefix {
        let close = value[open + prefix_len..]
            .find(']')
            .map(|relative| relative + open + prefix_len);
        return Some(MacroToken::Formula(FormulaToken {
            open,
            content_start: open + prefix_len,
            content_end: close.unwrap_or(value.len()),
            end: close.map_or(value.len(), |close| close + 1),
            closed: close.is_some(),
        }));
    }
    if let Some(short_reference) = rest.strip_prefix("<<") {
        let close = open + 2 + short_reference.find(">>")?;
        return Some(MacroToken::Reference(ReferenceToken::Short {
            open,
            target_start: open + 2,
            close,
            end: close + 2,
        }));
    }
    if starts_ascii_case_insensitive(rest, "xref:") {
        let target_start = open + 5;
        let bracket = target_start + value[target_start..].find('[')?;
        if value[target_start..bracket]
            .chars()
            .any(char::is_whitespace)
        {
            return None;
        }
        let close = bracket + 1 + value[bracket + 1..].find(']')?;
        return Some(MacroToken::Reference(ReferenceToken::Xref {
            open,
            target_start,
            bracket,
            close,
            end: close + 1,
        }));
    }
    if starts_ascii_case_insensitive(rest, "link:") {
        let target_start = open + 5;
        let bracket = target_start + value[target_start..].find('[')?;
        if value[target_start..bracket]
            .chars()
            .any(char::is_whitespace)
        {
            return None;
        }
        let close = bracket + 1 + value[bracket + 1..].find(']')?;
        return Some(MacroToken::Link(LinkToken::Explicit {
            open,
            target_start,
            bracket,
            close,
            end: close + 1,
        }));
    }

    let scheme_end = url_scheme_end(rest)?;
    let relative_target_end = rest
        .char_indices()
        .find_map(|(offset, character)| {
            (offset > scheme_end && (character.is_whitespace() || character == '['))
                .then_some(offset)
        })
        .unwrap_or(rest.len());
    let mut target_end = open + relative_target_end;
    while target_end > open
        && matches!(
            value[..target_end].chars().next_back(),
            Some('.' | ',' | ';')
        )
    {
        target_end -= 1;
    }
    if target_end <= open + scheme_end {
        return None;
    }
    let (label, end) = if value.as_bytes().get(target_end) == Some(&b'[') {
        let close = target_end + 1 + value[target_end + 1..].find(']')?;
        (Some((target_end + 1, close)), close + 1)
    } else {
        (None, target_end)
    };
    Some(MacroToken::Link(LinkToken::Url {
        open,
        target_end,
        label,
        end,
    }))
}

fn build_macro(
    value: &str,
    range: TextRange,
    config: InlineParseConfig,
    depth: usize,
    token: MacroToken,
) -> BuiltInline {
    match token {
        MacroToken::Formula(FormulaToken {
            open,
            content_start,
            content_end,
            end,
            closed,
        }) => {
            let formula = InlineFormula {
                range: subrange(range, open, end),
                content_range: subrange(range, content_start, content_end),
                language: MathLanguage::Latex,
                value: value[content_start..content_end].to_owned(),
                closed,
            };
            let mut problems = Vec::new();
            if !formula.closed {
                problems.push(InlineProblem {
                    kind: InlineProblemKind::UnclosedStem,
                    range: formula.range,
                });
            }
            if formula.value.is_empty() {
                problems.push(InlineProblem {
                    kind: InlineProblemKind::EmptyStem,
                    range: formula.content_range,
                });
            }
            if formula.value.len() > config.max_formula_bytes {
                problems.push(InlineProblem {
                    kind: InlineProblemKind::StemSizeLimitExceeded,
                    range: formula.content_range,
                });
            }
            BuiltInline {
                inline: Inline::Formula(formula),
                end,
                problems,
            }
        }
        MacroToken::Reference(token) => build_reference_macro(value, range, config, depth, token),
        MacroToken::Link(token) => build_link_macro(value, range, config, depth, token),
    }
}

fn build_reference_macro(
    value: &str,
    range: TextRange,
    config: InlineParseConfig,
    depth: usize,
    token: ReferenceToken,
) -> BuiltInline {
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
                )
            });
            let (label_inlines, problems) = label_output.map_or_else(
                || (Vec::new(), Vec::new()),
                |output| (output.inlines, output.problems),
            );
            BuiltInline {
                inline: Inline::Reference(Reference {
                    range: subrange(range, open, end),
                    target_range,
                    target_source: anchor.to_owned(),
                    destination: if anchor.is_empty() {
                        ReferenceDestination::Invalid
                    } else {
                        ReferenceDestination::Local {
                            anchor: anchor.to_owned(),
                            anchor_range: target_range,
                        }
                    },
                    label_range,
                    label: label_inlines,
                }),
                end,
                problems,
            }
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
            let label = parse_segment(label_text, label_range, config, depth + 1);
            BuiltInline {
                inline: Inline::Reference(Reference {
                    range: subrange(range, open, end),
                    target_range,
                    target_source: target.to_owned(),
                    destination: parse_reference_destination(target, target_range),
                    label_range: Some(label_range),
                    label: label.inlines,
                }),
                end,
                problems: label.problems,
            }
        }
    }
}

fn build_link_macro(
    value: &str,
    range: TextRange,
    config: InlineParseConfig,
    depth: usize,
    token: LinkToken,
) -> BuiltInline {
    match token {
        LinkToken::Explicit {
            open,
            target_start,
            bracket,
            close,
            end,
        } => {
            let target_range = subrange(range, target_start, bracket);
            let label_range = subrange(range, bracket + 1, close);
            let target = value[target_start..bracket].to_owned();
            let label = parse_segment(&value[bracket + 1..close], label_range, config, depth + 1);
            BuiltInline {
                inline: Inline::Link(Link {
                    range: subrange(range, open, end),
                    target_range,
                    target_attributes: attribute_uses(&target, target_range),
                    target_source: target.clone(),
                    target,
                    label_range: Some(label_range),
                    label: label.inlines,
                }),
                end,
                problems: label.problems,
            }
        }
        LinkToken::Url {
            open,
            target_end,
            label: label_offsets,
            end,
        } => {
            let (label_range, label, problems) =
                label_offsets.map_or((None, Vec::new(), Vec::new()), |(start, close)| {
                    let label_range = subrange(range, start, close);
                    let output =
                        parse_segment(&value[start..close], label_range, config, depth + 1);
                    (Some(label_range), output.inlines, output.problems)
                });
            let target_range = subrange(range, open, target_end);
            BuiltInline {
                inline: Inline::Link(Link {
                    range: subrange(range, open, end),
                    target_range,
                    target_source: value[open..target_end].to_owned(),
                    target: value[open..target_end].to_owned(),
                    target_attributes: attribute_uses(&value[open..target_end], target_range),
                    label_range,
                    label,
                }),
                end,
                problems,
            }
        }
    }
}

fn attribute_uses(value: &str, range: TextRange) -> Vec<AttributeUse> {
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

fn scan_incomplete_macros(value: &str, range: TextRange, problems: &mut Vec<InlineProblem>) {
    for (offset, _) in value.char_indices() {
        if is_escaped(value, offset) {
            continue;
        }
        let rest = &value[offset..];
        let bracket_is_unclosed = |candidate: &str| {
            candidate
                .find('[')
                .is_some_and(|open| !candidate[open + 1..].contains(']'))
        };
        let kind = if (rest.starts_with("<<") && !rest.contains(">>"))
            || (starts_ascii_case_insensitive(rest, "xref:")
                && (rest.find('[').is_none() || bracket_is_unclosed(rest)))
        {
            Some(InlineProblemKind::IncompleteCrossReference)
        } else if !starts_ascii_case_insensitive(rest, "stem:[")
            && !starts_ascii_case_insensitive(rest, "latexmath:[")
            && is_token_boundary(value[..offset].chars().next_back())
            && url_scheme_end(rest).is_some()
            && bracket_is_unclosed(rest)
        {
            Some(InlineProblemKind::IncompleteLink)
        } else {
            None
        };
        if let Some(kind) = kind {
            problems.push(InlineProblem {
                kind,
                range: subrange(range, offset, value.len()),
            });
            break;
        }
    }
}

fn url_scheme_end(value: &str) -> Option<usize> {
    let colon = value.find(':')?;
    let scheme = &value[..colon];
    if scheme.is_empty()
        || !scheme.as_bytes()[0].is_ascii_alphabetic()
        || !scheme
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.' | b'%'))
        || scheme.eq_ignore_ascii_case("xref")
    {
        None
    } else {
        Some(colon + 1)
    }
}

fn starts_ascii_case_insensitive(value: &str, prefix: &str) -> bool {
    value
        .get(..prefix.len())
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix))
}

fn is_token_boundary(previous: Option<char>) -> bool {
    previous.is_none_or(|character| {
        character.is_whitespace() || matches!(character, '(' | '[' | '{' | '<' | '"' | '\'')
    })
}

fn is_escaped(value: &str, offset: usize) -> bool {
    value[..offset]
        .chars()
        .rev()
        .take_while(|character| *character == '\\')
        .count()
        % 2
        == 1
}

fn next_opener(value: &str, cursor: usize) -> Option<(usize, char)> {
    value[cursor..]
        .char_indices()
        .map(|(offset, marker)| (cursor + offset, marker))
        .find(|(offset, marker)| {
            *marker == '{'
                || matches!(marker, '`' | '*' | '_') && is_open_boundary(value, *offset, *marker)
        })
}

fn find_closer(value: &str, open: usize, marker: char) -> Option<usize> {
    if marker == '{' {
        return value[open + 1..].find('}').map(|offset| open + 1 + offset);
    }
    value[open + marker.len_utf8()..]
        .char_indices()
        .map(|(offset, candidate)| (open + marker.len_utf8() + offset, candidate))
        .find(|(offset, candidate)| {
            *candidate == marker && is_close_boundary(value, *offset, marker)
        })
        .map(|(offset, _)| offset)
}

fn is_open_boundary(value: &str, offset: usize, marker: char) -> bool {
    let previous = value[..offset].chars().next_back();
    let next = value[offset + marker.len_utf8()..].chars().next();
    next.is_some_and(|character| !character.is_whitespace() && character != marker)
        && previous.is_none_or(|character| !character.is_alphanumeric())
}

fn is_close_boundary(value: &str, offset: usize, marker: char) -> bool {
    let previous = value[..offset].chars().next_back();
    let next = value[offset + marker.len_utf8()..].chars().next();
    previous.is_some_and(|character| !character.is_whitespace() && character != marker)
        && next.is_none_or(|character| !character.is_alphanumeric())
}

fn push_text(inlines: &mut Vec<Inline>, value: &str, range: TextRange, start: usize, end: usize) {
    if start != end {
        inlines.push(Inline::Text(InlineText {
            range: subrange(range, start, end),
            value: value[start..end].to_owned(),
        }));
    }
}

fn subrange(parent: TextRange, start: usize, end: usize) -> TextRange {
    let base = parent.start().to_usize();
    TextRange::new(
        TextSize::new(base + start).expect("inline offset fits"),
        TextSize::new(base + end).expect("inline offset fits"),
    )
    .expect("inline range is ordered")
}

pub fn inline_at(inlines: &[Inline], offset: u32) -> Option<&Inline> {
    inlines.iter().find_map(|inline| {
        let range = inline.range();
        if range.start().to_u32() <= offset && offset < range.end().to_u32() {
            match inline {
                Inline::Styled { children, .. } => inline_at(children, offset).or(Some(inline)),
                _ => Some(inline),
            }
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{
        FormulaToken, Inline, InlineCandidate, InlineLiteralKind, InlineParseConfig,
        InlineProblemKind, InlineStyle, LinkToken, MacroToken, MarkerRecognition, MarkerToken,
        ReferenceDestination, ReferenceToken, inline_at, next_candidate, parse, parse_text,
        recognize_macro, recognize_marker,
    };
    use crate::source::{TextRange, TextSize};

    fn range(start: usize, end: usize) -> TextRange {
        TextRange::new(
            TextSize::new(start).expect("small offset"),
            TextSize::new(end).expect("small offset"),
        )
        .expect("ordered range")
    }

    #[test]
    fn inline_text_preserves_source_range_and_unicode() {
        let inlines = parse_text("日本語 😀", range(4, 18), InlineParseConfig::default());
        let Inline::Text(text) = &inlines[0] else {
            panic!("expected text");
        };
        assert_eq!(text.value, "日本語 😀");
        assert_eq!(text.range, range(4, 18));
        assert_eq!(inline_at(&inlines, 6), Some(&inlines[0]));
        assert_eq!(inline_at(&inlines, 18), None);
    }

    #[test]
    fn inline_text_handles_empty_input() {
        assert!(parse_text("", range(0, 0), InlineParseConfig::default()).is_empty());
    }

    #[test]
    fn recognizer_orders_macros_and_markers_by_source_position() {
        assert_eq!(
            next_candidate("*strong* https://example.org", 0),
            Some(InlineCandidate::Marker {
                open: 0,
                marker: '*'
            })
        );
        assert_eq!(
            next_candidate("https://example.org *strong*", 0),
            Some(InlineCandidate::Macro { open: 0 })
        );
        assert_eq!(
            next_candidate("日本語 xref:other.adoc[]", "日本語 ".len()),
            Some(InlineCandidate::Macro {
                open: "日本語 ".len()
            })
        );
    }

    #[test]
    fn macro_recognizer_returns_ranges_without_building_nodes() {
        assert!(matches!(
            recognize_macro("stem:[x]", 0),
            Some(MacroToken::Formula(FormulaToken {
                content_start: 6,
                content_end: 7,
                end: 8,
                closed: true,
                ..
            }))
        ));
        assert!(matches!(
            recognize_macro("<<id,label>>", 0),
            Some(MacroToken::Reference(ReferenceToken::Short {
                target_start: 2,
                close: 10,
                end: 12,
                ..
            }))
        ));
        assert!(matches!(
            recognize_macro("xref:other.adoc[Other]", 0),
            Some(MacroToken::Reference(ReferenceToken::Xref {
                target_start: 5,
                bracket: 15,
                close: 21,
                end: 22,
                ..
            }))
        ));
        assert!(matches!(
            recognize_macro("https://example.org[label]", 0),
            Some(MacroToken::Link(LinkToken::Url {
                target_end: 19,
                label: Some((20, 25)),
                end: 26,
                ..
            }))
        ));
    }

    #[test]
    fn marker_recognizer_distinguishes_complete_invalid_and_unclosed_input() {
        assert_eq!(
            recognize_marker("*strong*", 0, '*'),
            MarkerRecognition::Complete(MarkerToken {
                open: 0,
                close: 7,
                end: 8,
                marker: '*',
            })
        );
        assert_eq!(
            recognize_marker("{bad name}", 0, '{'),
            MarkerRecognition::Invalid { next: 1 }
        );
        assert_eq!(
            recognize_marker("_open", 0, '_'),
            MarkerRecognition::Unclosed {
                next: 1,
                kind: InlineProblemKind::UnclosedEmphasis,
            }
        );
    }

    #[test]
    fn candidate_recovery_always_advances_on_utf8_boundaries() {
        let source = "日本語 xref:broken[ *open _also";
        let mut cursor = 0;
        let mut steps = 0;
        while let Some(candidate) = next_candidate(source, cursor) {
            let open = match candidate {
                InlineCandidate::Macro { open } | InlineCandidate::Marker { open, .. } => open,
            };
            let next = super::next_char_boundary(source, open);
            assert!(next > cursor);
            assert!(source.is_char_boundary(next));
            cursor = next;
            steps += 1;
        }
        assert!(steps <= source.chars().count());
    }

    #[test]
    fn links_keep_target_label_and_source_ranges_separate() {
        let source = "see https://example.com[*site*].";
        let output = parse(source, range(10, 42), InlineParseConfig::default());
        let Inline::Link(link) = &output.inlines[1] else {
            panic!("expected link");
        };
        assert_eq!(link.target_source, "https://example.com");
        assert_eq!(link.target, "https://example.com");
        assert_eq!(
            &source[link.target_range.start().to_usize() - 10
                ..link.target_range.end().to_usize() - 10],
            "https://example.com"
        );
        assert!(matches!(
            link.label[0],
            Inline::Styled {
                style: InlineStyle::Strong,
                ..
            }
        ));
        assert!(output.problems.is_empty());
    }

    #[test]
    fn macro_labels_propagate_nested_inline_problems() {
        for (source, expected) in [
            (
                "https://example.com[*open]",
                InlineProblemKind::UnclosedStrong,
            ),
            (
                "xref:other.adoc[_open]",
                InlineProblemKind::UnclosedEmphasis,
            ),
            ("<<target,`open>>", InlineProblemKind::UnclosedMonospace),
        ] {
            let output = parse(source, range(0, source.len()), InlineParseConfig::default());
            assert!(
                output
                    .problems
                    .iter()
                    .any(|problem| problem.kind == expected),
                "missing {expected:?} for {source:?}"
            );
        }
    }

    #[test]
    fn escaped_macros_do_not_report_literal_contents_as_syntax() {
        let source = "\\stem:[";
        let output = parse(source, range(0, source.len()), InlineParseConfig::default());

        assert!(output.problems.is_empty());
        assert!(matches!(
            output.inlines.as_slice(),
            [Inline::Text(text)] if text.value == "stem:["
        ));
    }

    #[test]
    fn cross_references_share_one_typed_model() {
        let source = concat!(
            "<<local,Local>> ",
            "xref:#local[] ",
            "xref:other.adoc#part[Other] ",
            "xref:note:123#part[Note]"
        );
        let output = parse(source, range(0, source.len()), InlineParseConfig::default());
        let references = output
            .inlines
            .iter()
            .filter_map(|inline| match inline {
                Inline::Reference(reference) => Some(reference),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(references.len(), 4);
        assert!(matches!(
            references[0].destination,
            ReferenceDestination::Local { ref anchor, .. } if anchor == "local"
        ));
        assert!(matches!(
            references[2].destination,
            ReferenceDestination::Document { ref document, ref anchor, .. }
                if document == "other.adoc" && anchor.as_deref() == Some("part")
        ));
        assert!(matches!(
            references[3].destination,
            ReferenceDestination::Scheme { ref scheme, ref locator, .. }
                if scheme == "note" && locator == "123"
        ));
    }

    #[test]
    fn links_and_cross_references_support_backslash_escape_and_recovery() {
        let source = "\\https://example.com[x] xref:broken[ then `code`";
        let output = parse(source, range(0, source.len()), InlineParseConfig::default());
        let visible_text = output
            .inlines
            .iter()
            .filter_map(|inline| match inline {
                Inline::Text(text) => Some(text.value.as_str()),
                _ => None,
            })
            .collect::<String>();
        assert_eq!(visible_text, "https://example.com[x] xref:broken[ then ");
        assert!(output.inlines.iter().any(|inline| matches!(
            inline,
            Inline::Literal { value, .. } if value == "code"
        )));
        assert!(
            output
                .problems
                .iter()
                .any(|problem| problem.kind == InlineProblemKind::IncompleteCrossReference)
        );
    }

    #[test]
    fn incomplete_macro_detection_ignores_brackets_before_the_macro() {
        let source = "] https://example.com[open";
        let output = parse(source, range(0, source.len()), InlineParseConfig::default());

        assert!(
            output
                .problems
                .iter()
                .any(|problem| problem.kind == InlineProblemKind::IncompleteLink)
        );
    }

    #[test]
    fn monospace_parses_multiple_spans_and_ranges() {
        let output = parse(
            "a `one` and `二`",
            range(10, 27),
            InlineParseConfig::default(),
        );
        assert_eq!(output.inlines.len(), 4);
        assert!(matches!(
            &output.inlines[1],
            Inline::Literal {
                kind: InlineLiteralKind::Monospace,
                value,
                ..
            } if value == "one"
        ));
        assert!(matches!(
            &output.inlines[3],
            Inline::Literal {
                kind: InlineLiteralKind::Monospace,
                value,
                ..
            } if value == "二"
        ));
        assert!(output.problems.is_empty());
    }

    #[test]
    fn monospace_unclosed_input_recovers_as_text() {
        let output = parse("before `open", range(0, 12), InlineParseConfig::default());
        assert_eq!(output.inlines.len(), 1);
        assert!(matches!(&output.inlines[0], Inline::Text(text) if text.value == "before `open"));
        assert_eq!(
            output.problems[0].kind,
            InlineProblemKind::UnclosedMonospace
        );
        assert_eq!(output.problems[0].range, range(7, 8));
    }

    #[test]
    fn monospace_requires_constrained_boundaries() {
        let output = parse(
            "word`code`word and ``",
            range(0, 20),
            InlineParseConfig::default(),
        );
        assert!(
            output
                .inlines
                .iter()
                .all(|inline| matches!(inline, Inline::Text(_)))
        );
        assert!(output.problems.is_empty());
    }

    #[test]
    fn strong_parses_content_and_nested_monospace() {
        let output = parse(
            "a *strong `code` text* end",
            range(0, 26),
            InlineParseConfig::default(),
        );
        let Inline::Styled {
            style: InlineStyle::Strong,
            children,
            ..
        } = &output.inlines[1]
        else {
            panic!("expected strong");
        };
        assert!(children.iter().any(|inline| matches!(
            inline,
            Inline::Literal {
                kind: InlineLiteralKind::Monospace,
                value,
                ..
            } if value == "code"
        )));
        assert!(output.problems.is_empty());
    }

    #[test]
    fn strong_unclosed_marker_does_not_hide_later_monospace() {
        let output = parse(
            "*open then `code`",
            range(0, 17),
            InlineParseConfig::default(),
        );
        assert!(output.inlines.iter().any(|inline| matches!(
            inline,
            Inline::Literal {
                kind: InlineLiteralKind::Monospace,
                ..
            }
        )));
        assert!(
            output
                .problems
                .iter()
                .any(|problem| problem.kind == InlineProblemKind::UnclosedStrong)
        );
    }

    #[test]
    fn strong_handles_multiple_spans_and_leaves_empty_markers_as_text() {
        let output = parse(
            "*one* and *two* plus **",
            range(0, 23),
            InlineParseConfig::default(),
        );

        assert_eq!(
            output
                .inlines
                .iter()
                .filter(|inline| matches!(
                    inline,
                    Inline::Styled {
                        style: InlineStyle::Strong,
                        ..
                    }
                ))
                .count(),
            2
        );
        assert!(matches!(
            output.inlines.last(),
            Some(Inline::Text(text)) if text.value.ends_with("plus **")
        ));
    }

    #[test]
    fn emphasis_parses_combinations_and_ignores_identifier_underscores() {
        let source = "_italic *bold `code`*_ and some_identifier";
        let output = parse(source, range(0, source.len()), InlineParseConfig::default());
        let Inline::Styled {
            style: InlineStyle::Emphasis,
            children,
            ..
        } = &output.inlines[0]
        else {
            panic!("expected emphasis");
        };
        assert!(matches!(
            children[1],
            Inline::Styled {
                style: InlineStyle::Strong,
                ..
            }
        ));
        assert!(matches!(
            output.inlines.last(),
            Some(Inline::Text(text)) if text.value.ends_with("some_identifier")
        ));
        assert!(output.problems.is_empty());
    }

    #[test]
    fn inline_recovery_keeps_safe_spans_after_unclosed_emphasis() {
        let source = "_open then *strong* and `code`";
        let output = parse(source, range(0, source.len()), InlineParseConfig::default());
        assert!(output.inlines.iter().any(|inline| matches!(
            inline,
            Inline::Styled {
                style: InlineStyle::Strong,
                ..
            }
        )));
        assert!(output.inlines.iter().any(|inline| matches!(
            inline,
            Inline::Literal {
                kind: InlineLiteralKind::Monospace,
                ..
            }
        )));
        assert!(
            output
                .problems
                .iter()
                .any(|problem| problem.kind == InlineProblemKind::UnclosedEmphasis)
        );
    }

    #[test]
    fn inline_recovery_reports_nesting_limit_and_keeps_source_text() {
        let source = "*outer _inner_*";
        let output = parse(
            source,
            range(0, source.len()),
            InlineParseConfig {
                max_depth: 1,
                ..InlineParseConfig::default()
            },
        );
        let Inline::Styled {
            style: InlineStyle::Strong,
            children,
            ..
        } = &output.inlines[0]
        else {
            panic!("expected outer strong");
        };
        assert!(matches!(
            &children[1],
            Inline::Text(text) if text.value == "_inner_"
        ));
        assert!(
            output
                .problems
                .iter()
                .any(|problem| problem.kind == InlineProblemKind::NestingLimitExceeded)
        );
    }
}
