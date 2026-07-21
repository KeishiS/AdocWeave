//! Output-independent inline syntax.

use crate::budget::{BudgetExceeded, ParseBudget};
use crate::source::{TextRange, TextSize};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InlineText {
    pub range: TextRange,
    pub value: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Link {
    pub range: TextRange,
    pub target_range: TextRange,
    pub target_source: String,
    pub target: String,
    pub target_attributes: Vec<AttributeUse>,
    pub target_expansion_error: Option<crate::substitution::AttributeExpansionError>,
    pub label_range: Option<TextRange>,
    pub label: Vec<Inline>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttributeUse {
    pub name: String,
    pub name_range: TextRange,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum StandardMacroKind {
    Email,
    Footnote,
    Anchor,
    BibliographyAnchor,
    IndexTerm,
    Keyboard,
    Button,
    Menu,
    Image,
    Icon,
    Audio,
    Video,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum MacroForm {
    Inline,
    Block,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MacroAttribute {
    pub range: TextRange,
    pub name: Option<String>,
    pub value: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StandardMacro {
    pub kind: StandardMacroKind,
    pub form: MacroForm,
    pub range: TextRange,
    pub target_range: TextRange,
    pub target_source: String,
    pub target: String,
    pub target_attributes: Vec<AttributeUse>,
    pub target_expansion_error: Option<crate::substitution::AttributeExpansionError>,
    pub attributes_range: TextRange,
    pub attributes: Vec<MacroAttribute>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MathLanguage {
    Latex,
    Typst,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InlineFormula {
    pub range: TextRange,
    pub content_range: TextRange,
    pub language: MathLanguage,
    pub value: String,
    pub closed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reference {
    pub range: TextRange,
    pub target_range: TextRange,
    pub target_source: String,
    pub destination: ReferenceDestination,
    pub label_range: Option<TextRange>,
    pub label: Vec<Inline>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
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

#[derive(Clone, Debug, Eq, PartialEq)]
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
        value: Option<String>,
        expansion_error: Option<crate::substitution::AttributeExpansionError>,
    },
    Link(Link),
    Reference(Reference),
    Formula(InlineFormula),
    Macro(StandardMacro),
    Passthrough {
        kind: PassthroughKind,
        range: TextRange,
        content_range: TextRange,
        value: String,
    },
    HardBreak {
        range: TextRange,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InlineLiteralKind {
    Monospace,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PassthroughKind {
    SinglePlus,
    DoublePlus,
    TriplePlus,
    Macro,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InlineStyle {
    Strong,
    Emphasis,
    Highlight,
    Subscript,
    Superscript,
    CurvedDoubleQuote,
    CurvedSingleQuote,
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
            Self::Macro(node) => node.range,
            Self::Passthrough { range, .. } => *range,
            Self::HardBreak { range } => *range,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InlineProblemKind {
    UnclosedMonospace,
    UnclosedStrong,
    UnclosedEmphasis,
    UnclosedHighlight,
    UnclosedSubscript,
    UnclosedSuperscript,
    NestingLimitExceeded,
    UnclosedAttributeReference,
    IncompleteLink,
    UnclosedPassthrough,
    IncompleteCrossReference,
    InvalidCrossReference,
    UnclosedStem,
    EmptyStem,
    StemSizeLimitExceeded,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

#[cfg(test)]
pub(crate) fn parse(value: &str, range: TextRange, config: InlineParseConfig) -> InlineParseOutput {
    parse_with_budget(value, range, config, &mut ParseBudget::unlimited())
        .expect("the test and compatibility parser uses an unlimited budget")
}

pub(crate) fn parse_with_budget(
    value: &str,
    range: TextRange,
    config: InlineParseConfig,
    budget: &mut ParseBudget,
) -> Result<InlineParseOutput, BudgetExceeded> {
    parse_segment(value, range, config, 0, budget)
}

fn parse_segment(
    value: &str,
    range: TextRange,
    config: InlineParseConfig,
    depth: usize,
    budget: &mut ParseBudget,
) -> Result<InlineParseOutput, BudgetExceeded> {
    let mut output = InlineParseOutput::default();
    let mut cursor = 0;
    let mut plain_start = 0;
    let mut scanner = InlineScanner::new(value);

    while let Some(candidate) = scanner.next(cursor) {
        match candidate {
            InlineCandidate::EscapedAnchor { slash } => {
                push_text(
                    &mut output.inlines,
                    value,
                    range,
                    plain_start,
                    slash,
                    budget,
                )?;
                push_inline(
                    &mut output.inlines,
                    Inline::Text(InlineText {
                        range: subrange(range, slash, slash + 2),
                        value: "[".to_owned(),
                    }),
                    budget,
                )?;
                cursor = slash + 2;
                plain_start = cursor;
            }
            InlineCandidate::Macro { open } => {
                match scanner.recognize_macro(value, open) {
                    MacroRecognition::Complete(token) => {
                        if is_escaped(value, open) {
                            let end = token.end();
                            push_text(
                                &mut output.inlines,
                                value,
                                range,
                                plain_start,
                                open - 1,
                                budget,
                            )?;
                            push_inline(
                                &mut output.inlines,
                                Inline::Text(InlineText {
                                    range: subrange(range, open - 1, end),
                                    value: value[open..end].to_owned(),
                                }),
                                budget,
                            )?;
                            cursor = end;
                            plain_start = end;
                        } else {
                            let built = build_macro(value, range, config, depth, token, budget)?;
                            push_text(
                                &mut output.inlines,
                                value,
                                range,
                                plain_start,
                                open,
                                budget,
                            )?;
                            push_inline(&mut output.inlines, built.inline, budget)?;
                            cursor = built.end;
                            plain_start = built.end;
                            output.problems.extend(built.problems);
                        }
                    }
                    MacroRecognition::Incomplete { kind, next } => {
                        if is_escaped(value, open) {
                            push_text(
                                &mut output.inlines,
                                value,
                                range,
                                plain_start,
                                open - 1,
                                budget,
                            )?;
                            push_inline(
                                &mut output.inlines,
                                Inline::Text(InlineText {
                                    range: subrange(range, open - 1, value.len()),
                                    value: value[open..].to_owned(),
                                }),
                                budget,
                            )?;
                            cursor = value.len();
                            plain_start = cursor;
                        } else {
                            output.problems.push(InlineProblem {
                                kind,
                                range: subrange(range, open, value.len()),
                            });
                            cursor = next;
                        }
                    }
                    MacroRecognition::Invalid { next } => cursor = next,
                }
                if cursor == value.len() {
                    break;
                }
                if cursor > open {
                    continue;
                }
                cursor = next_char_boundary(value, open);
            }
            InlineCandidate::Marker {
                open,
                marker,
                form,
                close,
            } => {
                if is_escaped(value, open) {
                    let marker_width = form.width();
                    push_text(
                        &mut output.inlines,
                        value,
                        range,
                        plain_start,
                        open - 1,
                        budget,
                    )?;
                    push_inline(
                        &mut output.inlines,
                        Inline::Text(InlineText {
                            range: subrange(range, open - 1, open + marker_width),
                            value: value[open..open + marker_width].to_owned(),
                        }),
                        budget,
                    )?;
                    cursor = open + marker_width;
                    plain_start = cursor;
                    continue;
                }
                match recognize_marker(value, open, marker, form, close) {
                    MarkerRecognition::Complete(token) => {
                        let built = build_marker(value, range, config, depth, token, budget)?;
                        push_text(&mut output.inlines, value, range, plain_start, open, budget)?;
                        push_inline(&mut output.inlines, built.inline, budget)?;
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
            InlineCandidate::TypographicQuote {
                open,
                quote,
                content_start,
                content_end,
                end,
            } => {
                push_text(&mut output.inlines, value, range, plain_start, open, budget)?;
                let content_range = subrange(range, content_start, content_end);
                let inner = parse_segment(
                    &value[content_start..content_end],
                    content_range,
                    config,
                    depth.saturating_add(1),
                    budget,
                )?;
                output.problems.extend(inner.problems);
                push_inline(
                    &mut output.inlines,
                    Inline::Styled {
                        style: if quote == '"' {
                            InlineStyle::CurvedDoubleQuote
                        } else {
                            InlineStyle::CurvedSingleQuote
                        },
                        range: subrange(range, open, end),
                        content_range,
                        children: inner.inlines,
                    },
                    budget,
                )?;
                cursor = end;
                plain_start = end;
            }
            InlineCandidate::Passthrough {
                open,
                width,
                content_start,
                content_end,
                end,
            } => {
                push_text(&mut output.inlines, value, range, plain_start, open, budget)?;
                push_inline(
                    &mut output.inlines,
                    Inline::Passthrough {
                        kind: match width {
                            1 => PassthroughKind::SinglePlus,
                            2 => PassthroughKind::DoublePlus,
                            3 => PassthroughKind::TriplePlus,
                            _ => unreachable!(),
                        },
                        range: subrange(range, open, end),
                        content_range: subrange(range, content_start, content_end),
                        value: value[content_start..content_end].to_owned(),
                    },
                    budget,
                )?;
                cursor = end;
                plain_start = end;
            }
        }
    }

    push_text(
        &mut output.inlines,
        value,
        range,
        plain_start,
        value.len(),
        budget,
    )?;
    Ok(output)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InlineCandidate {
    EscapedAnchor {
        slash: usize,
    },
    Macro {
        open: usize,
    },
    Marker {
        open: usize,
        marker: char,
        form: MarkerForm,
        close: Option<usize>,
    },
    TypographicQuote {
        open: usize,
        quote: char,
        content_start: usize,
        content_end: usize,
        end: usize,
    },
    Passthrough {
        open: usize,
        width: usize,
        content_start: usize,
        content_end: usize,
        end: usize,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MarkerForm {
    Constrained,
    Unconstrained,
}

impl MarkerForm {
    const fn width(self) -> usize {
        match self {
            Self::Constrained => 1,
            Self::Unconstrained => 2,
        }
    }
}

struct InlineScanner {
    candidates: Vec<InlineCandidate>,
    delimiters: DelimiterIndex,
    next: usize,
    _inspected_positions: usize,
}

impl InlineScanner {
    fn new(value: &str) -> Self {
        let (mut candidates, mut preparsed_markers, mut inspected_positions) =
            preparsed_candidates(value);
        let unconstrained_pairs = index_unconstrained_pairs(value, &mut inspected_positions);
        for (open, marker) in value.char_indices() {
            inspected_positions += 1;
            let rest = &value[open..];
            if preparsed_markers[open] {
                continue;
            }
            if marker == '\\'
                && (rest.starts_with("\\[[") || rest.starts_with("\\[#"))
                && !is_escaped(value, open)
            {
                candidates.push(InlineCandidate::EscapedAnchor { slash: open });
                let end = if rest.starts_with("\\[[") {
                    rest.find("]]")
                        .map_or(value.len(), |close| open + close + 2)
                } else {
                    rest.find(']').map_or(value.len(), |close| open + close + 1)
                };
                for protected in preparsed_markers.iter_mut().take(end).skip(open) {
                    *protected = true;
                }
                continue;
            }
            let boundary = is_macro_boundary(value, open);
            let is_macro = rest.starts_with("<<")
                || rest.starts_with("[[")
                || boundary
                    && (starts_ascii_case_insensitive(rest, "xref:")
                        || starts_ascii_case_insensitive(rest, "stem:[")
                        || starts_ascii_case_insensitive(rest, "latexmath:[")
                        || starts_ascii_case_insensitive(rest, "pass:[")
                        || standard_macro_prefix(rest).is_some()
                        || email_address_end(rest).is_some()
                        || url_scheme_end(rest).is_some());
            if is_macro {
                candidates.push(InlineCandidate::Macro { open });
            } else if matches!(marker, '`' | '*' | '_' | '#') && unconstrained_pairs[open] {
                candidates.push(InlineCandidate::Marker {
                    open,
                    marker,
                    form: MarkerForm::Unconstrained,
                    close: None,
                });
            } else if marker == '{'
                || matches!(marker, '^' | '~')
                    && value[open + marker.len_utf8()..]
                        .chars()
                        .next()
                        .is_some_and(|character| !character.is_whitespace())
                || matches!(marker, '`' | '*' | '_' | '#') && is_open_boundary(value, open, marker)
            {
                candidates.push(InlineCandidate::Marker {
                    open,
                    marker,
                    form: MarkerForm::Constrained,
                    close: None,
                });
            }
        }
        index_marker_closers(
            value,
            &unconstrained_pairs,
            &mut candidates,
            &mut inspected_positions,
        );
        candidates.sort_by_key(|candidate| candidate.open());
        let delimiters = DelimiterIndex::new_counted(value, &mut inspected_positions);
        Self {
            candidates,
            delimiters,
            next: 0,
            _inspected_positions: inspected_positions,
        }
    }

    fn next(&mut self, cursor: usize) -> Option<InlineCandidate> {
        while self
            .candidates
            .get(self.next)
            .is_some_and(|candidate| candidate.open() < cursor)
        {
            self.next += 1;
        }
        let candidate = self.candidates.get(self.next).copied()?;
        self.next += 1;
        Some(candidate)
    }

    fn recognize_macro(&self, value: &str, open: usize) -> MacroRecognition {
        recognize_macro_with_index(value, open, &self.delimiters)
    }

    #[cfg(test)]
    fn inspected_positions(&self) -> usize {
        self._inspected_positions
    }
}

fn preparsed_candidates(value: &str) -> (Vec<InlineCandidate>, Vec<bool>, usize) {
    let mut candidates = Vec::new();
    let mut markers = vec![false; value.len() + 1];
    let mut next_plus = [
        vec![None; value.len() + 1],
        vec![None; value.len() + 1],
        vec![None; value.len() + 1],
    ];
    let mut next_double_quote = vec![None; value.len() + 1];
    let mut next_single_quote = vec![None; value.len() + 1];
    let mut plus = [None; 3];
    let mut double_quote = None;
    let mut single_quote = None;
    let bytes = value.as_bytes();
    let mut inspected_positions = 0;
    for offset in (0..value.len()).rev() {
        inspected_positions += 1;
        for width in 1..=3 {
            if bytes[offset..].starts_with(&[b'+'; 3][..width]) {
                plus[width - 1] = Some(offset);
            }
            next_plus[width - 1][offset] = plus[width - 1];
        }
        if bytes[offset..].starts_with(b"`\"") {
            double_quote = Some(offset);
        }
        if bytes[offset..].starts_with(b"`'") {
            single_quote = Some(offset);
        }
        next_double_quote[offset] = double_quote;
        next_single_quote[offset] = single_quote;
    }
    let mut cursor = 0;
    while cursor + 1 < value.len() {
        inspected_positions += 1;
        let quote = value[cursor..].chars().next().expect("cursor is in range");
        if quote == '+' {
            let run = value.as_bytes()[cursor..]
                .iter()
                .take_while(|byte| **byte == b'+')
                .count()
                .min(3);
            if run > 0 && (run > 1 || is_open_boundary(value, cursor, '+')) {
                let content_start = cursor + run;
                if let Some(content_end) = next_plus[run - 1][content_start] {
                    if content_end > content_start {
                        let end = content_end + run;
                        for marker in markers.iter_mut().skip(cursor).take(run) {
                            *marker = true;
                        }
                        for marker in markers.iter_mut().take(end).skip(content_end) {
                            *marker = true;
                        }
                        candidates.push(InlineCandidate::Passthrough {
                            open: cursor,
                            width: run,
                            content_start,
                            content_end,
                            end,
                        });
                        cursor = end;
                        continue;
                    }
                }
            }
        }
        if !matches!(quote, '\'' | '"') || value.as_bytes().get(cursor + 1) != Some(&b'`') {
            cursor += quote.len_utf8();
            continue;
        }
        let content_start = cursor + 2;
        let close = if quote == '"' {
            next_double_quote[content_start]
        } else {
            next_single_quote[content_start]
        };
        let Some(content_end) = close else {
            cursor = content_start;
            continue;
        };
        let end = content_end + 2;
        markers[cursor] = true;
        markers[cursor + 1] = true;
        markers[content_end] = true;
        markers[content_end + 1] = true;
        candidates.push(InlineCandidate::TypographicQuote {
            open: cursor,
            quote,
            content_start,
            content_end,
            end,
        });
        cursor = end;
    }
    (candidates, markers, inspected_positions)
}

struct DelimiterIndex {
    next_open_bracket: Vec<Option<usize>>,
    next_close_bracket: Vec<Option<usize>>,
    next_double_greater: Vec<Option<usize>>,
}

impl DelimiterIndex {
    #[cfg(test)]
    fn new(value: &str) -> Self {
        let mut ignored = 0;
        Self::new_counted(value, &mut ignored)
    }

    fn new_counted(value: &str, inspected_positions: &mut usize) -> Self {
        let mut next_open_bracket = vec![None; value.len() + 1];
        let mut next_close_bracket = vec![None; value.len() + 1];
        let mut next_double_greater = vec![None; value.len() + 1];
        let mut open_bracket = None;
        let mut close_bracket = None;
        let mut double_greater = None;
        for offset in (0..value.len()).rev() {
            *inspected_positions = (*inspected_positions).saturating_add(1);
            if value.as_bytes()[offset] == b'[' {
                open_bracket = Some(offset);
            }
            if value.as_bytes()[offset] == b']' {
                close_bracket = Some(offset);
            }
            if value.as_bytes()[offset] == b'>' && value.as_bytes().get(offset + 1) == Some(&b'>') {
                double_greater = Some(offset);
            }
            next_open_bracket[offset] = open_bracket;
            next_close_bracket[offset] = close_bracket;
            next_double_greater[offset] = double_greater;
        }
        Self {
            next_open_bracket,
            next_close_bracket,
            next_double_greater,
        }
    }
}

impl InlineCandidate {
    fn open(self) -> usize {
        match self {
            Self::EscapedAnchor { slash } => slash,
            Self::Macro { open }
            | Self::Marker { open, .. }
            | Self::TypographicQuote { open, .. }
            | Self::Passthrough { open, .. } => open,
        }
    }
}

#[cfg(test)]
fn next_candidate(value: &str, cursor: usize) -> Option<InlineCandidate> {
    InlineScanner::new(value).next(cursor)
}

fn next_char_boundary(value: &str, offset: usize) -> usize {
    offset + value[offset..].chars().next().map_or(1, char::len_utf8)
}

fn index_unconstrained_pairs(value: &str, inspected_positions: &mut usize) -> Vec<bool> {
    let bytes = value.as_bytes();
    let mut pairs = vec![false; bytes.len() + 1];
    let mut cursor = 0;
    while cursor < bytes.len() {
        *inspected_positions = (*inspected_positions).saturating_add(1);
        let marker = bytes[cursor];
        if !matches!(marker, b'`' | b'*' | b'_' | b'#') {
            cursor += 1;
            continue;
        }
        let mut run_end = cursor + 1;
        while bytes.get(run_end) == Some(&marker) {
            *inspected_positions = (*inspected_positions).saturating_add(1);
            run_end += 1;
        }
        let mut pair = cursor;
        while pair + 1 < run_end {
            pairs[pair] = true;
            pair += 2;
        }
        cursor = run_end;
    }
    pairs
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MarkerToken {
    open: usize,
    close: usize,
    end: usize,
    marker: char,
    form: MarkerForm,
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

fn recognize_marker(
    value: &str,
    open: usize,
    marker: char,
    form: MarkerForm,
    close: Option<usize>,
) -> MarkerRecognition {
    let width = form.width();
    let next = open + width;
    let Some(close) = close else {
        if form == MarkerForm::Unconstrained
            && (next == value.len()
                || value[next..]
                    .chars()
                    .next()
                    .is_some_and(char::is_whitespace))
        {
            return MarkerRecognition::Invalid { next };
        }
        let kind = match marker {
            '`' => InlineProblemKind::UnclosedMonospace,
            '*' => InlineProblemKind::UnclosedStrong,
            '_' => InlineProblemKind::UnclosedEmphasis,
            '#' => InlineProblemKind::UnclosedHighlight,
            '~' => InlineProblemKind::UnclosedSubscript,
            '^' => InlineProblemKind::UnclosedSuperscript,
            '{' => InlineProblemKind::UnclosedAttributeReference,
            _ => unreachable!("only supported markers are returned"),
        };
        return MarkerRecognition::Unclosed { next, kind };
    };
    if close == next {
        return MarkerRecognition::Invalid {
            next: close + width,
        };
    }
    if marker == '{' && !valid_attribute_name(&value[next..close]) {
        return MarkerRecognition::Invalid { next };
    }
    if matches!(marker, '^' | '~') && value[next..close].chars().any(char::is_whitespace) {
        return MarkerRecognition::Invalid { next };
    }
    MarkerRecognition::Complete(MarkerToken {
        open,
        close,
        end: close + width,
        marker,
        form,
    })
}

fn index_marker_closers(
    value: &str,
    unconstrained_pairs: &[bool],
    candidates: &mut [InlineCandidate],
    inspected_positions: &mut usize,
) {
    let mut opener_at = vec![None; value.len() + 1];
    for candidate in candidates.iter() {
        *inspected_positions = (*inspected_positions).saturating_add(1);
        if let InlineCandidate::Marker {
            open, marker, form, ..
        } = candidate
        {
            opener_at[*open] = Some((*marker, *form));
        }
    }

    let mut closer_at = vec![None; value.len() + 1];
    let mut last_backtick = None;
    let mut last_strong = None;
    let mut last_emphasis = None;
    let mut last_highlight = None;
    let mut last_subscript = None;
    let mut last_superscript = None;
    let mut last_unconstrained_backtick = None;
    let mut last_unconstrained_strong = None;
    let mut last_unconstrained_emphasis = None;
    let mut last_unconstrained_highlight = None;
    let mut last_attribute = None;
    for (offset, marker) in value.char_indices().rev() {
        *inspected_positions = (*inspected_positions).saturating_add(1);
        if let Some((marker, form)) = opener_at[offset] {
            closer_at[offset] = match (marker, form) {
                ('`', MarkerForm::Constrained) => last_backtick,
                ('*', MarkerForm::Constrained) => last_strong,
                ('_', MarkerForm::Constrained) => last_emphasis,
                ('#', MarkerForm::Constrained) => last_highlight,
                ('~', MarkerForm::Constrained) => last_subscript,
                ('^', MarkerForm::Constrained) => last_superscript,
                ('`', MarkerForm::Unconstrained) => last_unconstrained_backtick,
                ('*', MarkerForm::Unconstrained) => last_unconstrained_strong,
                ('_', MarkerForm::Unconstrained) => last_unconstrained_emphasis,
                ('#', MarkerForm::Unconstrained) => last_unconstrained_highlight,
                ('{', MarkerForm::Constrained) => last_attribute,
                _ => None,
            };
        }
        if unconstrained_pairs[offset] {
            match marker {
                '`' => last_unconstrained_backtick = Some(offset),
                '*' => last_unconstrained_strong = Some(offset),
                '_' => last_unconstrained_emphasis = Some(offset),
                '#' => last_unconstrained_highlight = Some(offset),
                _ => {}
            }
        }
        match marker {
            '`' if is_close_boundary(value, offset, marker) => last_backtick = Some(offset),
            '*' if is_close_boundary(value, offset, marker) => last_strong = Some(offset),
            '_' if is_close_boundary(value, offset, marker) => last_emphasis = Some(offset),
            '#' if is_close_boundary(value, offset, marker) => last_highlight = Some(offset),
            '~' => last_subscript = Some(offset),
            '^' => last_superscript = Some(offset),
            '}' => last_attribute = Some(offset),
            _ => {}
        }
    }

    for candidate in candidates {
        *inspected_positions = (*inspected_positions).saturating_add(1);
        if let InlineCandidate::Marker { open, close, .. } = candidate {
            *close = closer_at[*open];
        }
    }
}

fn build_marker(
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

fn valid_attribute_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MacroToken {
    Formula(FormulaToken),
    Reference(ReferenceToken),
    Link(LinkToken),
    Passthrough(PassthroughToken),
    Standard(StandardMacroToken),
    ShorthandAnchor(ShorthandAnchorToken),
    Email(EmailToken),
}

impl MacroToken {
    const fn end(self) -> usize {
        match self {
            Self::Formula(token) => token.end,
            Self::Reference(ReferenceToken::Short { end, .. })
            | Self::Reference(ReferenceToken::Xref { end, .. })
            | Self::Link(LinkToken::Explicit { end, .. })
            | Self::Link(LinkToken::Url { end, .. }) => end,
            Self::Passthrough(token) => token.end,
            Self::Standard(token) => token.end,
            Self::ShorthandAnchor(token) => token.end,
            Self::Email(token) => token.end,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MacroRecognition {
    Complete(MacroToken),
    Incomplete {
        kind: InlineProblemKind,
        next: usize,
    },
    Invalid {
        next: usize,
    },
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
struct PassthroughToken {
    open: usize,
    content_start: usize,
    content_end: usize,
    end: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StandardMacroToken {
    kind: StandardMacroKind,
    form: MacroForm,
    open: usize,
    target_start: usize,
    bracket: usize,
    close: usize,
    end: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ShorthandAnchorToken {
    kind: StandardMacroKind,
    open: usize,
    target_start: usize,
    target_end: usize,
    end: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EmailToken {
    open: usize,
    end: usize,
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

fn standard_macro_prefix(value: &str) -> Option<(StandardMacroKind, MacroForm, usize)> {
    use StandardMacroKind as Kind;
    const PREFIXES: &[(&str, Kind, MacroForm)] = &[
        ("image::", Kind::Image, MacroForm::Block),
        ("icon::", Kind::Icon, MacroForm::Block),
        ("audio::", Kind::Audio, MacroForm::Block),
        ("video::", Kind::Video, MacroForm::Block),
        ("footnote:", Kind::Footnote, MacroForm::Inline),
        ("anchor:", Kind::Anchor, MacroForm::Inline),
        ("bibanchor:", Kind::BibliographyAnchor, MacroForm::Inline),
        ("indexterm:", Kind::IndexTerm, MacroForm::Inline),
        ("kbd:", Kind::Keyboard, MacroForm::Inline),
        ("btn:", Kind::Button, MacroForm::Inline),
        ("menu:", Kind::Menu, MacroForm::Inline),
        ("image:", Kind::Image, MacroForm::Inline),
        ("icon:", Kind::Icon, MacroForm::Inline),
        ("audio:", Kind::Audio, MacroForm::Inline),
        ("video:", Kind::Video, MacroForm::Inline),
    ];
    PREFIXES.iter().find_map(|(prefix, kind, form)| {
        starts_ascii_case_insensitive(value, prefix).then_some((*kind, *form, prefix.len()))
    })
}

fn email_address_end(value: &str) -> Option<usize> {
    let token_end = value
        .char_indices()
        .find_map(|(offset, character)| character.is_whitespace().then_some(offset))
        .unwrap_or(value.len());
    let candidate = value[..token_end].trim_end_matches(['.', ',', ';', ':']);
    let at = candidate.find('@')?;
    if at == 0
        || !candidate[..at]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-'))
    {
        return None;
    }
    let domain_end = candidate[at + 1..]
        .char_indices()
        .find_map(|(offset, character)| {
            (!character.is_ascii_alphanumeric() && !matches!(character, '.' | '-'))
                .then_some(at + 1 + offset)
        })
        .unwrap_or(candidate.len());
    let domain = &candidate[at + 1..domain_end];
    (domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.ends_with('-'))
    .then_some(domain_end)
}

fn recognize_macro_with_index(
    value: &str,
    open: usize,
    delimiters: &DelimiterIndex,
) -> MacroRecognition {
    let rest = &value[open..];
    if let Some(content) = rest.strip_prefix("[[[") {
        if let Some(relative_end) = content.find("]]]") {
            let target_end = open + 3 + relative_end;
            return MacroRecognition::Complete(MacroToken::ShorthandAnchor(ShorthandAnchorToken {
                kind: StandardMacroKind::BibliographyAnchor,
                open,
                target_start: open + 3,
                target_end,
                end: target_end + 3,
            }));
        }
    }
    if let Some(content) = rest.strip_prefix("[[") {
        if let Some(relative_end) = content.find("]]") {
            let target_end = open + 2 + relative_end;
            return MacroRecognition::Complete(MacroToken::ShorthandAnchor(ShorthandAnchorToken {
                kind: StandardMacroKind::Anchor,
                open,
                target_start: open + 2,
                target_end,
                end: target_end + 2,
            }));
        }
    }
    let formula_prefix = if starts_ascii_case_insensitive(rest, "stem:[") {
        Some("stem:[".len())
    } else if starts_ascii_case_insensitive(rest, "latexmath:[") {
        Some("latexmath:[".len())
    } else {
        None
    };
    if let Some(prefix_len) = formula_prefix {
        let close = delimiters.next_close_bracket[open + prefix_len];
        return MacroRecognition::Complete(MacroToken::Formula(FormulaToken {
            open,
            content_start: open + prefix_len,
            content_end: close.unwrap_or(value.len()),
            end: close.map_or(value.len(), |close| close + 1),
            closed: close.is_some(),
        }));
    }
    if starts_ascii_case_insensitive(rest, "pass:[") {
        let content_start = open + "pass:[".len();
        let Some(close) = delimiters.next_close_bracket[content_start] else {
            return MacroRecognition::Incomplete {
                kind: InlineProblemKind::UnclosedPassthrough,
                next: next_char_boundary(value, open),
            };
        };
        return MacroRecognition::Complete(MacroToken::Passthrough(PassthroughToken {
            open,
            content_start,
            content_end: close,
            end: close + 1,
        }));
    }
    if rest.starts_with("<<") {
        let Some(close) = delimiters.next_double_greater[open + 2] else {
            return MacroRecognition::Incomplete {
                kind: InlineProblemKind::IncompleteCrossReference,
                next: next_char_boundary(value, open),
            };
        };
        return MacroRecognition::Complete(MacroToken::Reference(ReferenceToken::Short {
            open,
            target_start: open + 2,
            close,
            end: close + 2,
        }));
    }
    if starts_ascii_case_insensitive(rest, "xref:") {
        let target_start = open + 5;
        let Some(bracket) = delimiters.next_open_bracket[target_start] else {
            return MacroRecognition::Incomplete {
                kind: InlineProblemKind::IncompleteCrossReference,
                next: next_char_boundary(value, open),
            };
        };
        if value[target_start..bracket]
            .chars()
            .any(char::is_whitespace)
        {
            return MacroRecognition::Invalid {
                next: next_char_boundary(value, open),
            };
        }
        let Some(close) = delimiters.next_close_bracket[bracket + 1] else {
            return MacroRecognition::Incomplete {
                kind: InlineProblemKind::IncompleteCrossReference,
                next: next_char_boundary(value, open),
            };
        };
        return MacroRecognition::Complete(MacroToken::Reference(ReferenceToken::Xref {
            open,
            target_start,
            bracket,
            close,
            end: close + 1,
        }));
    }
    if starts_ascii_case_insensitive(rest, "link:") {
        let target_start = open + 5;
        let Some(bracket) = delimiters.next_open_bracket[target_start] else {
            return MacroRecognition::Incomplete {
                kind: InlineProblemKind::IncompleteLink,
                next: next_char_boundary(value, open),
            };
        };
        if value[target_start..bracket]
            .chars()
            .any(char::is_whitespace)
        {
            return MacroRecognition::Invalid {
                next: next_char_boundary(value, open),
            };
        }
        let Some(close) = delimiters.next_close_bracket[bracket + 1] else {
            return MacroRecognition::Incomplete {
                kind: InlineProblemKind::IncompleteLink,
                next: next_char_boundary(value, open),
            };
        };
        return MacroRecognition::Complete(MacroToken::Link(LinkToken::Explicit {
            open,
            target_start,
            bracket,
            close,
            end: close + 1,
        }));
    }

    if let Some((kind, form, prefix_len)) = standard_macro_prefix(rest) {
        let target_start = open + prefix_len;
        let Some(bracket) = delimiters.next_open_bracket[target_start] else {
            return MacroRecognition::Incomplete {
                kind: InlineProblemKind::IncompleteLink,
                next: next_char_boundary(value, open),
            };
        };
        if value[target_start..bracket]
            .chars()
            .any(char::is_whitespace)
        {
            return MacroRecognition::Invalid {
                next: next_char_boundary(value, open),
            };
        }
        let Some(close) = delimiters.next_close_bracket[bracket + 1] else {
            return MacroRecognition::Incomplete {
                kind: InlineProblemKind::IncompleteLink,
                next: next_char_boundary(value, open),
            };
        };
        return MacroRecognition::Complete(MacroToken::Standard(StandardMacroToken {
            kind,
            form,
            open,
            target_start,
            bracket,
            close,
            end: close + 1,
        }));
    }

    if let Some(relative_end) = email_address_end(rest) {
        return MacroRecognition::Complete(MacroToken::Email(EmailToken {
            open,
            end: open + relative_end,
        }));
    }

    let Some(scheme_end) = url_scheme_end(rest) else {
        return MacroRecognition::Invalid {
            next: next_char_boundary(value, open),
        };
    };
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
        return MacroRecognition::Invalid {
            next: next_char_boundary(value, open),
        };
    }
    let (label, end) = if value.as_bytes().get(target_end) == Some(&b'[') {
        let Some(close) = delimiters.next_close_bracket[target_end + 1] else {
            return MacroRecognition::Incomplete {
                kind: InlineProblemKind::IncompleteLink,
                next: next_char_boundary(value, open),
            };
        };
        (Some((target_end + 1, close)), close + 1)
    } else {
        (None, target_end)
    };
    MacroRecognition::Complete(MacroToken::Link(LinkToken::Url {
        open,
        target_end,
        label,
        end,
    }))
}

#[cfg(test)]
fn recognize_macro(value: &str, open: usize) -> MacroRecognition {
    recognize_macro_with_index(value, open, &DelimiterIndex::new(value))
}

fn build_macro(
    value: &str,
    range: TextRange,
    config: InlineParseConfig,
    depth: usize,
    token: MacroToken,
    budget: &mut ParseBudget,
) -> Result<BuiltInline, BudgetExceeded> {
    match token {
        MacroToken::Passthrough(PassthroughToken {
            open,
            content_start,
            content_end,
            end,
        }) => Ok(BuiltInline {
            inline: Inline::Passthrough {
                kind: PassthroughKind::Macro,
                range: subrange(range, open, end),
                content_range: subrange(range, content_start, content_end),
                value: value[content_start..content_end].to_owned(),
            },
            end,
            problems: Vec::new(),
        }),
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
            Ok(BuiltInline {
                inline: Inline::Formula(formula),
                end,
                problems,
            })
        }
        MacroToken::Reference(token) => {
            build_reference_macro(value, range, config, depth, token, budget)
        }
        MacroToken::Link(token) => build_link_macro(value, range, config, depth, token, budget),
        MacroToken::Standard(token) => Ok(build_standard_macro(value, range, token)),
        MacroToken::ShorthandAnchor(token) => Ok(build_shorthand_anchor(value, range, token)),
        MacroToken::Email(token) => Ok(build_email(value, range, token)),
    }
}

fn build_shorthand_anchor(
    value: &str,
    range: TextRange,
    token: ShorthandAnchorToken,
) -> BuiltInline {
    let empty = subrange(range, token.target_end, token.target_end);
    BuiltInline {
        inline: Inline::Macro(StandardMacro {
            kind: token.kind,
            form: MacroForm::Inline,
            range: subrange(range, token.open, token.end),
            target_range: subrange(range, token.target_start, token.target_end),
            target_source: value[token.target_start..token.target_end].to_owned(),
            target: value[token.target_start..token.target_end].to_owned(),
            target_attributes: Vec::new(),
            target_expansion_error: None,
            attributes_range: empty,
            attributes: Vec::new(),
        }),
        end: token.end,
        problems: Vec::new(),
    }
}

fn build_email(value: &str, range: TextRange, token: EmailToken) -> BuiltInline {
    let target = &value[token.open..token.end];
    let empty = subrange(range, token.end, token.end);
    BuiltInline {
        inline: Inline::Macro(StandardMacro {
            kind: StandardMacroKind::Email,
            form: MacroForm::Inline,
            range: subrange(range, token.open, token.end),
            target_range: subrange(range, token.open, token.end),
            target_source: target.to_owned(),
            target: target.to_owned(),
            target_attributes: Vec::new(),
            target_expansion_error: None,
            attributes_range: empty,
            attributes: Vec::new(),
        }),
        end: token.end,
        problems: Vec::new(),
    }
}

fn build_standard_macro(value: &str, range: TextRange, token: StandardMacroToken) -> BuiltInline {
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
            let (name, item_value) = item.split_once('=').map_or((None, item), |(name, value)| {
                (Some(name.trim().to_owned()), value.trim())
            });
            attributes.push(MacroAttribute {
                range: subrange(range, item_start, item_end),
                name,
                value: item_value.trim_matches(['\'', '"']).to_owned(),
            });
        }
        start = offset + 1;
    }
    attributes
}

fn build_reference_macro(
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
                    target_range,
                    target_source: target.to_owned(),
                    destination: parse_reference_destination(target, target_range),
                    label_range: Some(label_range),
                    label: label.inlines,
                }),
                end,
                problems: label.problems,
            })
        }
    }
}

fn build_link_macro(
    value: &str,
    range: TextRange,
    config: InlineParseConfig,
    depth: usize,
    token: LinkToken,
    budget: &mut ParseBudget,
) -> Result<BuiltInline, BudgetExceeded> {
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
            let label = parse_segment(
                &value[bracket + 1..close],
                label_range,
                config,
                depth + 1,
                budget,
            )?;
            Ok(BuiltInline {
                inline: Inline::Link(Link {
                    range: subrange(range, open, end),
                    target_range,
                    target_attributes: attribute_uses(&target, target_range),
                    target_expansion_error: None,
                    target_source: target.clone(),
                    target,
                    label_range: Some(label_range),
                    label: label.inlines,
                }),
                end,
                problems: label.problems,
            })
        }
        LinkToken::Url {
            open,
            target_end,
            label: label_offsets,
            end,
        } => {
            let (label_range, label, problems) = match label_offsets {
                Some((start, close)) => {
                    let label_range = subrange(range, start, close);
                    let output = parse_segment(
                        &value[start..close],
                        label_range,
                        config,
                        depth + 1,
                        budget,
                    )?;
                    (Some(label_range), output.inlines, output.problems)
                }
                None => (None, Vec::new(), Vec::new()),
            };
            let target_range = subrange(range, open, target_end);
            Ok(BuiltInline {
                inline: Inline::Link(Link {
                    range: subrange(range, open, end),
                    target_range,
                    target_source: value[open..target_end].to_owned(),
                    target: value[open..target_end].to_owned(),
                    target_attributes: attribute_uses(&value[open..target_end], target_range),
                    target_expansion_error: None,
                    label_range,
                    label,
                }),
                end,
                problems,
            })
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

fn url_scheme_end(value: &str) -> Option<usize> {
    let colon = value.char_indices().find_map(|(offset, character)| {
        if character == ':' {
            Some(Some(offset))
        } else if character.is_whitespace() || matches!(character, '[' | ']' | '<' | '>') {
            Some(None)
        } else {
            None
        }
    })??;
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

fn is_macro_boundary(value: &str, offset: usize) -> bool {
    is_token_boundary(value[..offset].chars().next_back())
        || (is_escaped(value, offset)
            && is_token_boundary(value[..offset.saturating_sub(1)].chars().next_back()))
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

fn push_text(
    inlines: &mut Vec<Inline>,
    value: &str,
    range: TextRange,
    start: usize,
    end: usize,
    budget: &mut ParseBudget,
) -> Result<(), BudgetExceeded> {
    if start != end {
        push_inline(
            inlines,
            Inline::Text(InlineText {
                range: subrange(range, start, end),
                value: value[start..end].to_owned(),
            }),
            budget,
        )?;
    }
    Ok(())
}

fn push_inline(
    inlines: &mut Vec<Inline>,
    inline: Inline,
    budget: &mut ParseBudget,
) -> Result<(), BudgetExceeded> {
    budget.consume_node()?;
    inlines.push(inline);
    Ok(())
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
        InlineProblemKind, InlineScanner, InlineStyle, LinkToken, MacroForm, MacroRecognition,
        MacroToken, MarkerForm, MarkerRecognition, MarkerToken, ReferenceDestination,
        ReferenceToken, StandardMacroKind, inline_at, next_candidate, parse, parse_text,
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
                marker: '*',
                form: MarkerForm::Constrained,
                close: Some(7),
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
    fn scanner_has_a_fixed_linear_inspection_budget() {
        assert_eq!(InlineScanner::new("abc").inspected_positions(), 17);

        let source = "日本語 *open xref:broken[ https://example.org[label] _tail";
        let scanner = InlineScanner::new(source);

        assert!(scanner.inspected_positions() > source.len());
        assert!(scanner.inspected_positions() <= source.len() * 8);

        for repetitions in 1..128 {
            let hostile = "xref:".repeat(repetitions) + "target[open";
            let scanner = InlineScanner::new(&hostile);
            assert!(scanner.inspected_positions() <= hostile.len() * 8);
            let output = parse(
                &hostile,
                range(0, hostile.len()),
                InlineParseConfig::default(),
            );
            assert!(output.problems.len() <= 1);
        }
        for repetitions in 1..128 {
            let hostile = "\"`x ".repeat(repetitions);
            let scanner = InlineScanner::new(&hostile);
            assert!(
                scanner.inspected_positions() <= hostile.len() * 8,
                "preparsed quote indexing must remain linear"
            );
        }
    }

    #[test]
    fn macro_recognizer_returns_ranges_without_building_nodes() {
        assert!(matches!(
            recognize_macro("stem:[x]", 0),
            MacroRecognition::Complete(MacroToken::Formula(FormulaToken {
                content_start: 6,
                content_end: 7,
                end: 8,
                closed: true,
                ..
            }))
        ));
        assert!(matches!(
            recognize_macro("<<id,label>>", 0),
            MacroRecognition::Complete(MacroToken::Reference(ReferenceToken::Short {
                target_start: 2,
                close: 10,
                end: 12,
                ..
            }))
        ));
        assert!(matches!(
            recognize_macro("xref:other.adoc[Other]", 0),
            MacroRecognition::Complete(MacroToken::Reference(ReferenceToken::Xref {
                target_start: 5,
                bracket: 15,
                close: 21,
                end: 22,
                ..
            }))
        ));
        assert!(matches!(
            recognize_macro("https://example.org[label]", 0),
            MacroRecognition::Complete(MacroToken::Link(LinkToken::Url {
                target_end: 19,
                label: Some((20, 25)),
                end: 26,
                ..
            }))
        ));
        assert_eq!(
            recognize_macro("xref:other.adoc[open", 0),
            MacroRecognition::Incomplete {
                kind: InlineProblemKind::IncompleteCrossReference,
                next: 1,
            }
        );
        assert_eq!(
            recognize_macro("https://example.org[open", 0),
            MacroRecognition::Incomplete {
                kind: InlineProblemKind::IncompleteLink,
                next: 1,
            }
        );
    }

    #[test]
    fn marker_recognizer_distinguishes_complete_invalid_and_unclosed_input() {
        assert_eq!(
            recognize_marker("*strong*", 0, '*', MarkerForm::Constrained, Some(7),),
            MarkerRecognition::Complete(MarkerToken {
                open: 0,
                close: 7,
                end: 8,
                marker: '*',
                form: MarkerForm::Constrained,
            })
        );
        assert_eq!(
            recognize_marker("{bad name}", 0, '{', MarkerForm::Constrained, Some(9),),
            MarkerRecognition::Invalid { next: 1 }
        );
        assert_eq!(
            recognize_marker("_open", 0, '_', MarkerForm::Constrained, None),
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
            let open = candidate.open();
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
        for (source, expected) in [("\\stem:[", "stem:["), ("\\xref:broken[", "xref:broken[")] {
            let output = parse(source, range(0, source.len()), InlineParseConfig::default());
            assert!(output.problems.is_empty());
            assert!(matches!(
                output.inlines.as_slice(),
                [Inline::Text(text)] if text.value == expected
            ));
        }
    }

    #[test]
    fn escaped_markers_are_literal_without_the_escape_character() {
        for (source, expected) in [
            ("\\*strong*", "*strong*"),
            ("\\_emphasis_", "_emphasis_"),
            ("\\`mono`", "`mono`"),
            ("\\{name}", "{name}"),
            ("before \\*open", "before *open"),
        ] {
            let output = parse(source, range(0, source.len()), InlineParseConfig::default());
            let visible = output
                .inlines
                .iter()
                .map(|inline| match inline {
                    Inline::Text(text) => text.value.as_str(),
                    _ => panic!("escaped syntax must remain text: {source}"),
                })
                .collect::<String>();
            assert_eq!(visible, expected);
            assert!(output.problems.is_empty());
        }
    }

    #[test]
    fn escaped_anchor_openers_are_literal_text() {
        for (source, expected) in [("\\[[id]]", "[[id]]"), ("\\[#id]", "[#id]")] {
            let output = parse(source, range(0, source.len()), InlineParseConfig::default());
            let visible = output
                .inlines
                .iter()
                .map(|inline| match inline {
                    Inline::Text(text) => text.value.as_str(),
                    _ => panic!("escaped anchor must remain text"),
                })
                .collect::<String>();
            assert_eq!(visible, expected);
            assert!(output.problems.is_empty());
        }
    }

    #[test]
    fn backslash_runs_and_trailing_backslashes_recover_deterministically() {
        let trailing = parse("text\\", range(0, 5), InlineParseConfig::default());
        assert!(matches!(
            trailing.inlines.as_slice(),
            [Inline::Text(text)] if text.value == "text\\"
        ));

        let even = parse("\\\\*strong*", range(0, 10), InlineParseConfig::default());
        assert!(matches!(even.inlines[1], Inline::Styled { .. }));
        assert!(matches!(&even.inlines[0], Inline::Text(text) if text.value == "\\\\"));

        let odd = parse("\\\\\\*strong*", range(0, 11), InlineParseConfig::default());
        assert!(
            odd.inlines
                .iter()
                .all(|inline| matches!(inline, Inline::Text(_)))
        );
        let visible = odd
            .inlines
            .iter()
            .filter_map(|inline| match inline {
                Inline::Text(text) => Some(text.value.as_str()),
                _ => None,
            })
            .collect::<String>();
        assert_eq!(visible, "\\\\*strong*");
    }

    #[test]
    fn escapes_are_not_interpreted_inside_opaque_inline_contexts() {
        let source = "`\\*literal*` stem:[\\{x}]";
        let output = parse(source, range(0, source.len()), InlineParseConfig::default());

        assert!(matches!(
            &output.inlines[0],
            Inline::Literal { value, .. } if value == "\\*literal*"
        ));
        assert!(matches!(
            &output.inlines[2],
            Inline::Formula(formula) if formula.value == "\\{x}"
        ));
        assert!(output.problems.is_empty());
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
    fn standard_macros_share_target_attribute_and_range_model() {
        let source =
            "image::https://example.org/a.png[Alt,320,height=200] footnote:[note] user@example.org";
        let parsed = parse(source, range(0, source.len()), InlineParseConfig::default());
        let macros = parsed
            .inlines
            .iter()
            .filter_map(|inline| match inline {
                Inline::Macro(node) => Some(node),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(macros.len(), 3);
        assert_eq!(macros[0].kind, StandardMacroKind::Image);
        assert_eq!(macros[0].form, MacroForm::Block);
        assert_eq!(macros[0].attributes[0].value, "Alt");
        assert_eq!(macros[0].attributes[2].name.as_deref(), Some("height"));
        assert_eq!(macros[1].kind, StandardMacroKind::Footnote);
        assert_eq!(macros[2].kind, StandardMacroKind::Email);
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
    fn unconstrained_markers_work_inside_words_and_across_unicode_boundaries() {
        let source = "word**strong**word 日本語__強調__日本語 😀``code``😀";
        let output = parse(source, range(0, source.len()), InlineParseConfig::default());

        assert!(output.problems.is_empty());
        assert!(output.inlines.iter().any(|inline| {
            matches!(inline, Inline::Styled { style: InlineStyle::Strong, children, .. }
                if matches!(&children[..], [Inline::Text(text)] if text.value == "strong"))
        }));
        assert!(output.inlines.iter().any(|inline| {
            matches!(inline, Inline::Styled { style: InlineStyle::Emphasis, children, .. }
                if matches!(&children[..], [Inline::Text(text)] if text.value == "強調"))
        }));
        assert!(output.inlines.iter().any(|inline| {
            matches!(inline, Inline::Literal { kind: InlineLiteralKind::Monospace, value, .. }
                if value == "code")
        }));
    }

    #[test]
    fn unconstrained_styles_nest_and_adjacent_pairs_remain_deterministic() {
        let source = "**outer __inner__** **one****two**";
        let output = parse(source, range(0, source.len()), InlineParseConfig::default());

        assert!(output.problems.is_empty());
        let styled: Vec<_> = output
            .inlines
            .iter()
            .filter_map(|inline| match inline {
                Inline::Styled { children, .. } => Some(children),
                _ => None,
            })
            .collect();
        assert_eq!(styled.len(), 3);
        assert!(styled[0].iter().any(|inline| matches!(
            inline,
            Inline::Styled {
                style: InlineStyle::Emphasis,
                ..
            }
        )));
        assert!(matches!(&styled[1][..], [Inline::Text(text)] if text.value == "one"));
        assert!(matches!(&styled[2][..], [Inline::Text(text)] if text.value == "two"));
    }

    #[test]
    fn unconstrained_empty_and_escaped_pairs_stay_literal() {
        let source = "**** ____ `` \\**literal**";
        let output = parse(source, range(0, source.len()), InlineParseConfig::default());
        let visible = output
            .inlines
            .iter()
            .map(|inline| match inline {
                Inline::Text(text) => text.value.as_str(),
                _ => panic!("expected only literal text"),
            })
            .collect::<String>();

        assert_eq!(visible, "**** ____ `` **literal**");
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

    #[test]
    fn extended_quotes_and_passthroughs_build_typed_nodes() {
        let value = "#mark# H~2~O E=mc^2^ \"`double`\" '`single`' +*raw*+ pass:[_opaque_]";
        let parsed = parse(value, range(0, value.len()), InlineParseConfig::default());
        assert!(parsed.inlines.iter().any(|inline| matches!(
            inline,
            Inline::Styled {
                style: InlineStyle::Highlight,
                ..
            }
        )));
        assert!(parsed.inlines.iter().any(|inline| matches!(
            inline,
            Inline::Styled {
                style: InlineStyle::Subscript,
                ..
            }
        )));
        assert!(parsed.inlines.iter().any(|inline| matches!(
            inline,
            Inline::Styled {
                style: InlineStyle::Superscript,
                ..
            }
        )));
        assert!(parsed.inlines.iter().any(|inline| matches!(
            inline,
            Inline::Styled {
                style: InlineStyle::CurvedDoubleQuote,
                ..
            }
        )));
        assert!(parsed.inlines.iter().any(|inline| matches!(
            inline,
            Inline::Styled {
                style: InlineStyle::CurvedSingleQuote,
                ..
            }
        )));
        assert_eq!(
            parsed
                .inlines
                .iter()
                .filter(|inline| matches!(inline, Inline::Passthrough { .. }))
                .count(),
            2
        );
    }
}
