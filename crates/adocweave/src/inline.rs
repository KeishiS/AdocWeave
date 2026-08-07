//! Inline scanner, recognizers, and semantic builders.

mod lowering;

use crate::budget::{BudgetExceeded, ParseBudget};
use crate::inline_model::*;
use crate::limits::AnalysisLimits;
use crate::source::{TextRange, TextSize};

/// Returns whether a source fragment is exactly one plain inline text node.
///
/// This uses the same bounded recognizer as document parsing. Escapes, links,
/// macros, attribute references, styled text, code, pass-through content, and
/// formulas therefore return `false` without exposing parser implementation
/// types.
pub fn is_plain_inline_text(value: &str) -> bool {
    let limits = AnalysisLimits::default();
    if value.len() > limits.max_line_bytes as usize {
        return false;
    }
    let Ok(end) = TextSize::new(value.len()) else {
        return false;
    };
    let range = TextRange::new(TextSize::ZERO, end).expect("zero-to-length range is ordered");
    let Ok(mut budget) = ParseBudget::new(AnalysisLimits {
        max_nodes: 2,
        ..limits
    }) else {
        return false;
    };
    let Ok(output) =
        parse_with_budget_impl(value, range, InlineParseConfig::default(), &mut budget)
    else {
        return false;
    };
    matches!(
        output.inlines.as_slice(),
        [Inline::Text(text)] if output.problems.is_empty()
            && text.range == range
            && text.value == value
    )
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
    parse_with_budget_impl(value, range, config, &mut ParseBudget::unlimited())
        .expect("the test and compatibility parser uses an unlimited budget")
}

pub(crate) fn parse_with_budget_impl(
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
    let index = InlineCandidateIndex::new(value);
    let mut candidates = index.cursor();

    while let Some(candidate) = candidates.next(cursor) {
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
            candidate @ InlineCandidate::Macro { open } => {
                match index
                    .recognize(value, candidate)
                    .expect("macro candidates have recognition results")
                {
                    InlineRecognition::Matched(InlineToken::Macro(token)) => {
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
                            let built = lower_inline_token(
                                value,
                                range,
                                config,
                                depth,
                                InlineToken::Macro(token),
                                budget,
                            )?;
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
                    InlineRecognition::Recovered { kind, next, .. } => {
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
                    InlineRecognition::Rejected { next, .. } => cursor = next,
                    InlineRecognition::Matched(InlineToken::Marker(_)) => {
                        unreachable!("macro recognizer returns only macro tokens")
                    }
                }
                if cursor == value.len() {
                    break;
                }
                if cursor > open {
                    continue;
                }
                cursor = next_char_boundary(value, open);
            }
            candidate @ InlineCandidate::MacroBoundary { open } => {
                if let InlineRecognition::Matched(InlineToken::Macro(token)) = index
                    .recognize(value, candidate)
                    .expect("macro boundary candidates have recognition results")
                    && let Some((name_end, name)) = macro_boundary_subject(value, token)
                {
                    output.problems.push(InlineProblem {
                        kind: InlineProblemKind::MacroBoundary { name },
                        range: subrange(range, open, name_end),
                    });
                    cursor = token.end();
                } else {
                    cursor = next_char_boundary(value, open);
                }
            }
            candidate @ InlineCandidate::Marker { open, form, .. } => {
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
                match index
                    .recognize(value, candidate)
                    .expect("marker candidates have recognition results")
                {
                    InlineRecognition::Matched(InlineToken::Marker(token)) => {
                        let built = lower_inline_token(
                            value,
                            range,
                            config,
                            depth,
                            InlineToken::Marker(token),
                            budget,
                        )?;
                        push_text(&mut output.inlines, value, range, plain_start, open, budget)?;
                        push_inline(&mut output.inlines, built.inline, budget)?;
                        output.problems.extend(built.problems);
                        cursor = token.end;
                        plain_start = cursor;
                    }
                    InlineRecognition::Recovered { next, kind, .. } => {
                        output.problems.push(InlineProblem {
                            kind,
                            range: subrange(range, open, next),
                        });
                        cursor = next;
                    }
                    InlineRecognition::Rejected { next, .. } => cursor = next,
                    InlineRecognition::Matched(InlineToken::Macro(_)) => {
                        unreachable!("marker recognizer returns only marker tokens")
                    }
                }
            }
            InlineCandidate::MonospaceBoundary { open, end } => {
                output.problems.push(InlineProblem {
                    kind: InlineProblemKind::MonospaceBoundary,
                    range: subrange(range, open, end),
                });
                cursor = next_char_boundary(value, open);
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
    MacroBoundary {
        open: usize,
    },
    Marker {
        open: usize,
        marker: char,
        form: MarkerForm,
        close: Option<usize>,
    },
    MonospaceBoundary {
        open: usize,
        end: usize,
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

struct InlineCandidateIndex {
    candidates: Vec<InlineCandidate>,
    delimiters: DelimiterIndex,
    url_candidates: UrlCandidateIndex,
    #[cfg(test)]
    inspected_positions: usize,
}

impl InlineCandidateIndex {
    fn new(value: &str) -> Self {
        let (mut candidates, mut preparsed_markers, mut inspected_positions) =
            preparsed_candidates(value);
        index_invalid_monospace_boundaries(
            value,
            &mut preparsed_markers,
            &mut candidates,
            &mut inspected_positions,
        );
        let unconstrained_pairs = index_unconstrained_pairs(value, &mut inspected_positions);
        let url_candidates = UrlCandidateIndex::new(value, &mut inspected_positions);
        let mut rejected_macro_boundaries = Vec::new();
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
            let boundary_macro =
                macro_candidate(value, open, &url_candidates, &mut inspected_positions);
            let is_macro =
                rest.starts_with("<<") || rest.starts_with("[[") || boundary && boundary_macro;
            if is_macro {
                candidates.push(InlineCandidate::Macro { open });
            } else if boundary_macro && !is_escaped(value, open) {
                rejected_macro_boundaries.push(open);
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
        let delimiters = DelimiterIndex::new_counted(value, &mut inspected_positions);
        for open in rejected_macro_boundaries {
            candidates.push(InlineCandidate::MacroBoundary { open });
        }
        candidates.sort_by_key(|candidate| candidate.open());
        Self {
            candidates,
            delimiters,
            url_candidates,
            #[cfg(test)]
            inspected_positions,
        }
    }

    fn cursor(&self) -> InlineCandidateCursor<'_> {
        InlineCandidateCursor {
            candidates: &self.candidates,
            next: 0,
        }
    }

    fn recognize_macro(&self, value: &str, open: usize) -> InlineRecognition {
        recognize_macro_with_index(value, open, &self.delimiters, Some(&self.url_candidates))
    }

    fn recognize(&self, value: &str, candidate: InlineCandidate) -> Option<InlineRecognition> {
        match candidate {
            InlineCandidate::Macro { open } | InlineCandidate::MacroBoundary { open } => {
                Some(self.recognize_macro(value, open))
            }
            InlineCandidate::Marker {
                open,
                marker,
                form,
                close,
            } => Some(recognize_marker(value, open, marker, form, close)),
            InlineCandidate::EscapedAnchor { .. }
            | InlineCandidate::MonospaceBoundary { .. }
            | InlineCandidate::TypographicQuote { .. }
            | InlineCandidate::Passthrough { .. } => None,
        }
    }

    #[cfg(test)]
    fn inspected_positions(&self) -> usize {
        self.inspected_positions
    }

    #[cfg(test)]
    fn storage_bytes(&self) -> usize {
        self.candidates.capacity() * std::mem::size_of::<InlineCandidate>()
            + self.delimiters.storage_bytes()
            + self.url_candidates.storage_bytes()
    }
}

struct InlineCandidateCursor<'index> {
    candidates: &'index [InlineCandidate],
    next: usize,
}

impl InlineCandidateCursor<'_> {
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
}

fn preparsed_candidates(value: &str) -> (Vec<InlineCandidate>, Vec<bool>, usize) {
    assert_compact_offset_capacity(value.len());
    let mut candidates = Vec::new();
    let mut markers = vec![false; value.len() + 1];
    let mut next_plus = [
        CompactOffsetIndex::new(value.len() + 1),
        CompactOffsetIndex::new(value.len() + 1),
        CompactOffsetIndex::new(value.len() + 1),
    ];
    let mut next_double_quote = CompactOffsetIndex::new(value.len() + 1);
    let mut next_single_quote = CompactOffsetIndex::new(value.len() + 1);
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
            next_plus[width - 1].set(offset, plus[width - 1]);
        }
        if bytes[offset..].starts_with(b"`\"") {
            double_quote = Some(offset);
        }
        if bytes[offset..].starts_with(b"`'") {
            single_quote = Some(offset);
        }
        next_double_quote.set(offset, double_quote);
        next_single_quote.set(offset, single_quote);
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
                if let Some(content_end) = next_plus[run - 1].get(content_start)
                    && content_end > content_start
                {
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
        if !matches!(quote, '\'' | '"') || value.as_bytes().get(cursor + 1) != Some(&b'`') {
            cursor += quote.len_utf8();
            continue;
        }
        let content_start = cursor + 2;
        let close = if quote == '"' {
            next_double_quote.get(content_start)
        } else {
            next_single_quote.get(content_start)
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
    next_open_bracket: CompactOffsetIndex,
    next_close_bracket: CompactOffsetIndex,
    next_double_greater: CompactOffsetIndex,
}

impl DelimiterIndex {
    #[cfg(test)]
    fn new(value: &str) -> Self {
        let mut ignored = 0;
        Self::new_counted(value, &mut ignored)
    }

    fn new_counted(value: &str, inspected_positions: &mut usize) -> Self {
        assert_compact_offset_capacity(value.len());
        let mut next_open_bracket = CompactOffsetIndex::new(value.len() + 1);
        let mut next_close_bracket = CompactOffsetIndex::new(value.len() + 1);
        let mut next_double_greater = CompactOffsetIndex::new(value.len() + 1);
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
            next_open_bracket.set(offset, open_bracket);
            next_close_bracket.set(offset, close_bracket);
            next_double_greater.set(offset, double_greater);
        }
        Self {
            next_open_bracket,
            next_close_bracket,
            next_double_greater,
        }
    }

    fn next_open_bracket(&self, offset: usize) -> Option<usize> {
        self.next_open_bracket.get(offset)
    }

    fn next_close_bracket(&self, offset: usize) -> Option<usize> {
        self.next_close_bracket.get(offset)
    }

    fn next_double_greater(&self, offset: usize) -> Option<usize> {
        self.next_double_greater.get(offset)
    }

    #[cfg(test)]
    fn storage_bytes(&self) -> usize {
        self.next_open_bracket.storage_bytes()
            + self.next_close_bracket.storage_bytes()
            + self.next_double_greater.storage_bytes()
    }
}

const MISSING_COMPACT_OFFSET: u32 = u32::MAX;

struct CompactOffsetIndex(Vec<u32>);

impl CompactOffsetIndex {
    fn new(len: usize) -> Self {
        Self(vec![MISSING_COMPACT_OFFSET; len])
    }

    fn set(&mut self, index: usize, value: Option<usize>) {
        self.0[index] = value.map_or(MISSING_COMPACT_OFFSET, |offset| {
            u32::try_from(offset).expect("inline input fits compact offset index")
        });
    }

    fn get(&self, index: usize) -> Option<usize> {
        let offset = self.0[index];
        (offset != MISSING_COMPACT_OFFSET).then_some(offset as usize)
    }

    #[cfg(test)]
    fn storage_bytes(&self) -> usize {
        self.0.capacity() * std::mem::size_of::<u32>()
    }
}

fn assert_compact_offset_capacity(len: usize) {
    assert!(
        len < MISSING_COMPACT_OFFSET as usize,
        "inline input exceeds the 32-bit offset domain"
    );
}

impl InlineCandidate {
    fn open(self) -> usize {
        match self {
            Self::EscapedAnchor { slash } => slash,
            Self::Macro { open }
            | Self::MacroBoundary { open }
            | Self::Marker { open, .. }
            | Self::MonospaceBoundary { open, .. }
            | Self::TypographicQuote { open, .. }
            | Self::Passthrough { open, .. } => open,
        }
    }
}

fn index_invalid_monospace_boundaries(
    value: &str,
    protected: &mut [bool],
    candidates: &mut Vec<InlineCandidate>,
    inspected_positions: &mut usize,
) {
    let bytes = value.as_bytes();
    let mut cursor = 0;
    let mut pending = None;
    while cursor < bytes.len() {
        *inspected_positions = (*inspected_positions).saturating_add(1);
        if bytes[cursor] != b'`' {
            cursor += 1;
            continue;
        }
        let end = bytes[cursor..]
            .iter()
            .take_while(|byte| **byte == b'`')
            .count()
            + cursor;
        if end - cursor != 1 {
            pending = None;
            cursor = end;
            continue;
        }
        let Some(open) = pending.take() else {
            pending = Some(cursor);
            cursor = end;
            continue;
        };
        let close = cursor;
        cursor = end;
        if protected[open]
            || protected[close]
            || is_escaped(value, open)
            || is_escaped(value, close)
            || is_open_boundary(value, open, '`') && is_close_boundary(value, close, '`')
        {
            continue;
        }
        protected[open] = true;
        protected[close] = true;
        candidates.push(InlineCandidate::MonospaceBoundary {
            open,
            end: close + 1,
        });
    }
}

#[cfg(test)]
fn next_candidate(value: &str, cursor: usize) -> Option<InlineCandidate> {
    InlineCandidateIndex::new(value).cursor().next(cursor)
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
enum InlineToken {
    Macro(MacroToken),
    Marker(MarkerToken),
}

impl InlineToken {
    const fn open(self) -> usize {
        match self {
            Self::Macro(token) => token.open(),
            Self::Marker(token) => token.open,
        }
    }

    const fn end(self) -> usize {
        match self {
            Self::Macro(token) => token.end(),
            Self::Marker(token) => token.end,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InlineRecognition {
    Matched(InlineToken),
    Recovered {
        open: usize,
        next: usize,
        kind: InlineProblemKind,
    },
    Rejected {
        open: usize,
        next: usize,
    },
}

impl InlineRecognition {
    fn matched(value: &str, token: InlineToken) -> Self {
        let recognition = Self::Matched(token);
        debug_assert!(recognition.is_well_formed(value));
        recognition
    }

    fn recovered(value: &str, open: usize, next: usize, kind: InlineProblemKind) -> Self {
        let recognition = Self::Recovered { open, next, kind };
        debug_assert!(recognition.is_well_formed(value));
        recognition
    }

    fn rejected(value: &str, open: usize, next: usize) -> Self {
        let recognition = Self::Rejected { open, next };
        debug_assert!(recognition.is_well_formed(value));
        recognition
    }

    const fn open(self) -> usize {
        match self {
            Self::Matched(token) => token.open(),
            Self::Recovered { open, .. } | Self::Rejected { open, .. } => open,
        }
    }

    const fn next(self) -> usize {
        match self {
            Self::Matched(token) => token.end(),
            Self::Recovered { next, .. } | Self::Rejected { next, .. } => next,
        }
    }

    fn is_well_formed(self, value: &str) -> bool {
        let open = self.open();
        let next = self.next();
        open < next
            && next <= value.len()
            && value.is_char_boundary(open)
            && value.is_char_boundary(next)
    }
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
) -> InlineRecognition {
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
            return InlineRecognition::rejected(value, open, next);
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
        return InlineRecognition::recovered(value, open, next, kind);
    };
    if close == next {
        return InlineRecognition::rejected(value, open, close + width);
    }
    if marker == '{' && !valid_attribute_name(&value[next..close]) {
        return InlineRecognition::rejected(value, open, next);
    }
    if matches!(marker, '^' | '~') && value[next..close].chars().any(char::is_whitespace) {
        return InlineRecognition::rejected(value, open, next);
    }
    InlineRecognition::matched(
        value,
        InlineToken::Marker(MarkerToken {
            open,
            close,
            end: close + width,
            marker,
            form,
        }),
    )
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
    const fn open(self) -> usize {
        match self {
            Self::Formula(token) => token.open,
            Self::Reference(ReferenceToken::Short { open, .. })
            | Self::Reference(ReferenceToken::Xref { open, .. })
            | Self::Link(LinkToken::Explicit { open, .. })
            | Self::Link(LinkToken::Url { open, .. }) => open,
            Self::Passthrough(token) => token.open,
            Self::Standard(token) => token.open,
            Self::ShorthandAnchor(token) => token.open,
            Self::Email(token) => token.open,
        }
    }

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

fn macro_boundary_subject(value: &str, token: MacroToken) -> Option<(usize, &'static str)> {
    match token {
        MacroToken::Formula(token) => {
            let name = if starts_ascii_case_insensitive(&value[token.open..], "latexmath:[") {
                "latexmath"
            } else {
                "stem"
            };
            Some((token.content_start - 2, name))
        }
        MacroToken::Passthrough(token) => Some((token.content_start - 2, "pass")),
        MacroToken::Reference(ReferenceToken::Xref { target_start, .. }) => {
            Some((target_start - 1, "xref"))
        }
        MacroToken::Link(LinkToken::Explicit { target_start, .. }) => {
            Some((target_start - 1, "link"))
        }
        MacroToken::Link(LinkToken::Url { open, .. }) => {
            if starts_ascii_case_insensitive(&value[open..], "include::") {
                return None;
            }
            let scheme_end = url_scheme_end(&value[open..])?;
            Some((open + scheme_end - 1, "URL"))
        }
        MacroToken::Standard(StandardMacroToken {
            kind,
            form: MacroForm::Inline,
            target_start,
            ..
        }) => Some((target_start - 1, standard_macro_name(kind))),
        MacroToken::Email(token) => Some((token.end, "email")),
        MacroToken::Reference(ReferenceToken::Short { .. })
        | MacroToken::Standard(StandardMacroToken {
            form: MacroForm::Block,
            ..
        })
        | MacroToken::ShorthandAnchor(_) => None,
    }
}

const fn standard_macro_name(kind: StandardMacroKind) -> &'static str {
    use StandardMacroKind as Kind;
    match kind {
        Kind::Email => "email",
        Kind::Footnote => "footnote",
        Kind::Anchor => "anchor",
        Kind::BibliographyAnchor => "bibanchor",
        Kind::Citation => "cite",
        Kind::IndexTerm => "indexterm",
        Kind::Keyboard => "kbd",
        Kind::Button => "btn",
        Kind::Menu => "menu",
        Kind::Image => "image",
        Kind::Icon => "icon",
        Kind::Audio => "audio",
        Kind::Video => "video",
    }
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
    label: Option<(usize, usize)>,
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
        ("cite:", Kind::Citation, MacroForm::Inline),
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
    let at = value
        .bytes()
        .position(|byte| !email_local_part_byte(byte))
        .filter(|at| value.as_bytes()[*at] == b'@')?;
    if at == 0 {
        return None;
    }
    let mut domain_end = value[at + 1..]
        .bytes()
        .position(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-')))
        .map_or(value.len(), |offset| at + 1 + offset);
    while value.as_bytes().get(domain_end.saturating_sub(1)) == Some(&b'.') {
        domain_end -= 1;
    }
    let domain = &value[at + 1..domain_end];
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
    url_candidates: Option<&UrlCandidateIndex>,
) -> InlineRecognition {
    let rest = &value[open..];
    if let Some(content) = rest.strip_prefix("[[[")
        && let Some(relative_end) = content.find("]]]")
    {
        let close = open + 3 + relative_end;
        let (target_end, label) = split_anchor_label(value, open + 3, close);
        return InlineRecognition::matched(
            value,
            InlineToken::Macro(MacroToken::ShorthandAnchor(ShorthandAnchorToken {
                kind: StandardMacroKind::BibliographyAnchor,
                open,
                target_start: open + 3,
                target_end,
                label,
                end: close + 3,
            })),
        );
    }
    if let Some(content) = rest.strip_prefix("[[")
        && let Some(relative_end) = content.find("]]")
    {
        let close = open + 2 + relative_end;
        let (target_end, label) = split_anchor_label(value, open + 2, close);
        return InlineRecognition::matched(
            value,
            InlineToken::Macro(MacroToken::ShorthandAnchor(ShorthandAnchorToken {
                kind: StandardMacroKind::Anchor,
                open,
                target_start: open + 2,
                target_end,
                label,
                end: close + 2,
            })),
        );
    }
    let named_prefix = named_macro_prefix(rest);
    if let Some(NamedMacroPrefix::Formula { prefix_len }) = named_prefix {
        let close = delimiters.next_close_bracket(open + prefix_len);
        return InlineRecognition::matched(
            value,
            InlineToken::Macro(MacroToken::Formula(FormulaToken {
                open,
                content_start: open + prefix_len,
                content_end: close.unwrap_or(value.len()),
                end: close.map_or(value.len(), |close| close + 1),
                closed: close.is_some(),
            })),
        );
    }
    if let Some(NamedMacroPrefix::Passthrough { prefix_len }) = named_prefix {
        let content_start = open + prefix_len;
        let Some(close) = delimiters.next_close_bracket(content_start) else {
            return InlineRecognition::recovered(
                value,
                open,
                next_char_boundary(value, open),
                InlineProblemKind::UnclosedPassthrough,
            );
        };
        return InlineRecognition::matched(
            value,
            InlineToken::Macro(MacroToken::Passthrough(PassthroughToken {
                open,
                content_start,
                content_end: close,
                end: close + 1,
            })),
        );
    }
    if rest.starts_with("<<") {
        let Some(close) = delimiters.next_double_greater(open + 2) else {
            return InlineRecognition::recovered(
                value,
                open,
                next_char_boundary(value, open),
                InlineProblemKind::IncompleteCrossReference,
            );
        };
        return InlineRecognition::matched(
            value,
            InlineToken::Macro(MacroToken::Reference(ReferenceToken::Short {
                open,
                target_start: open + 2,
                close,
                end: close + 2,
            })),
        );
    }
    if let Some(NamedMacroPrefix::Xref { prefix_len }) = named_prefix {
        let target_start = open + prefix_len;
        let Some(bracket) = delimiters.next_open_bracket(target_start) else {
            return InlineRecognition::recovered(
                value,
                open,
                next_char_boundary(value, open),
                InlineProblemKind::IncompleteCrossReference,
            );
        };
        if value[target_start..bracket]
            .chars()
            .any(char::is_whitespace)
        {
            return InlineRecognition::rejected(value, open, next_char_boundary(value, open));
        }
        let Some(close) = delimiters.next_close_bracket(bracket + 1) else {
            return InlineRecognition::recovered(
                value,
                open,
                next_char_boundary(value, open),
                InlineProblemKind::IncompleteCrossReference,
            );
        };
        return InlineRecognition::matched(
            value,
            InlineToken::Macro(MacroToken::Reference(ReferenceToken::Xref {
                open,
                target_start,
                bracket,
                close,
                end: close + 1,
            })),
        );
    }
    if let Some(NamedMacroPrefix::Link { prefix_len }) = named_prefix {
        let target_start = open + prefix_len;
        let Some(bracket) = delimiters.next_open_bracket(target_start) else {
            return InlineRecognition::recovered(
                value,
                open,
                next_char_boundary(value, open),
                InlineProblemKind::IncompleteLink,
            );
        };
        if value[target_start..bracket]
            .chars()
            .any(char::is_whitespace)
        {
            return InlineRecognition::rejected(value, open, next_char_boundary(value, open));
        }
        let Some(close) = delimiters.next_close_bracket(bracket + 1) else {
            return InlineRecognition::recovered(
                value,
                open,
                next_char_boundary(value, open),
                InlineProblemKind::IncompleteLink,
            );
        };
        return InlineRecognition::matched(
            value,
            InlineToken::Macro(MacroToken::Link(LinkToken::Explicit {
                open,
                target_start,
                bracket,
                close,
                end: close + 1,
            })),
        );
    }

    if let Some(NamedMacroPrefix::Standard {
        kind,
        form,
        prefix_len,
    }) = named_prefix
    {
        let target_start = open + prefix_len;
        let Some(bracket) = delimiters.next_open_bracket(target_start) else {
            return InlineRecognition::recovered(
                value,
                open,
                next_char_boundary(value, open),
                InlineProblemKind::IncompleteLink,
            );
        };
        if value[target_start..bracket]
            .chars()
            .any(char::is_whitespace)
        {
            return InlineRecognition::rejected(value, open, next_char_boundary(value, open));
        }
        let Some(close) = delimiters.next_close_bracket(bracket + 1) else {
            return InlineRecognition::recovered(
                value,
                open,
                next_char_boundary(value, open),
                InlineProblemKind::IncompleteLink,
            );
        };
        return InlineRecognition::matched(
            value,
            InlineToken::Macro(MacroToken::Standard(StandardMacroToken {
                kind,
                form,
                open,
                target_start,
                bracket,
                close,
                end: close + 1,
            })),
        );
    }

    if let Some(relative_end) = email_address_end(rest) {
        return InlineRecognition::matched(
            value,
            InlineToken::Macro(MacroToken::Email(EmailToken {
                open,
                end: open + relative_end,
            })),
        );
    }

    let Some(scheme_end) = url_scheme_end(rest) else {
        return InlineRecognition::rejected(value, open, next_char_boundary(value, open));
    };
    let mut target_end = url_candidates.map_or_else(
        || {
            open + rest
                .char_indices()
                .find_map(|(offset, character)| {
                    (offset > scheme_end && (character.is_whitespace() || character == '['))
                        .then_some(offset)
                })
                .unwrap_or(rest.len())
        },
        |index| index.next_label_or_whitespace(open + scheme_end),
    );
    while target_end > open
        && matches!(
            value[..target_end].chars().next_back(),
            Some('.' | ',' | ';')
        )
    {
        target_end -= 1;
    }
    if target_end <= open + scheme_end {
        return InlineRecognition::rejected(value, open, next_char_boundary(value, open));
    }
    let (label, end) = if value.as_bytes().get(target_end) == Some(&b'[') {
        let Some(close) = delimiters.next_close_bracket(target_end + 1) else {
            return InlineRecognition::recovered(
                value,
                open,
                next_char_boundary(value, open),
                InlineProblemKind::IncompleteLink,
            );
        };
        (Some((target_end + 1, close)), close + 1)
    } else {
        (None, target_end)
    };
    InlineRecognition::matched(
        value,
        InlineToken::Macro(MacroToken::Link(LinkToken::Url {
            open,
            target_end,
            label,
            end,
        })),
    )
}

#[cfg(test)]
fn recognize_macro(value: &str, open: usize) -> InlineRecognition {
    recognize_macro_with_index(value, open, &DelimiterIndex::new(value), None)
}

fn lower_inline_token(
    value: &str,
    range: TextRange,
    config: InlineParseConfig,
    depth: usize,
    token: InlineToken,
    budget: &mut ParseBudget,
) -> Result<BuiltInline, BudgetExceeded> {
    match token {
        InlineToken::Macro(token) => build_macro(value, range, config, depth, token, budget),
        InlineToken::Marker(token) => {
            lowering::lower_marker(value, range, config, depth, token, budget)
        }
    }
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
            lowering::lower_reference(value, range, config, depth, token, budget)
        }
        MacroToken::Link(token) => build_link_macro(value, range, config, depth, token, budget),
        MacroToken::Standard(token) => Ok(lowering::lower_standard_macro(value, range, token)),
        MacroToken::ShorthandAnchor(token) => Ok(build_shorthand_anchor(value, range, token)),
        MacroToken::Email(token) => Ok(build_email(value, range, token)),
    }
}

/// Splits `[[id,xreftext]]` and `[[[id,xreftext]]]` at the first comma.
///
/// AsciiDoc gives an anchor its display text after a comma, and a numbered
/// citation style is written that way: `[[[smith2024,1]]]` shows as `1`.
/// Reading the comma as part of the identifier made a document written to the
/// language specification break, because `<<smith2024>>` then pointed at an
/// identifier nobody wrote. A block anchor already splits here, so the inline
/// and block forms now agree on where the identifier ends.
fn split_anchor_label(
    value: &str,
    target_start: usize,
    close: usize,
) -> (usize, Option<(usize, usize)>) {
    match value[target_start..close].find(',') {
        Some(offset) => {
            let comma = target_start + offset;
            (comma, Some((comma + 1, close)))
        }
        None => (close, None),
    }
}

fn build_shorthand_anchor(
    value: &str,
    range: TextRange,
    token: ShorthandAnchorToken,
) -> BuiltInline {
    let attributes_range = token.label.map_or_else(
        || subrange(range, token.target_end, token.target_end),
        |(start, end)| subrange(range, start, end),
    );
    let attributes = token.label.map_or_else(Vec::new, |(start, end)| {
        vec![MacroAttribute {
            range: subrange(range, start, end),
            value_range: subrange(range, start, end),
            name: None,
            value: value[start..end].to_owned(),
        }]
    });
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
            attributes_range,
            attributes,
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
                    macro_name_range: Some(subrange(range, open, target_start - 1)),
                    target_range,
                    target_attributes: lowering::attribute_uses(&target, target_range),
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
                    macro_name_range: None,
                    target_range,
                    target_source: value[open..target_end].to_owned(),
                    target: value[open..target_end].to_owned(),
                    target_attributes: lowering::attribute_uses(
                        &value[open..target_end],
                        target_range,
                    ),
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

fn url_scheme_end(value: &str) -> Option<usize> {
    let colon = value.char_indices().find_map(|(offset, character)| {
        if character == ':' {
            Some(Some(offset))
        } else if !character.is_ascii_alphanumeric() && !matches!(character, '+' | '-' | '.' | '%')
        {
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

struct UrlCandidateIndex {
    next_label_or_whitespace: Vec<u32>,
}

impl UrlCandidateIndex {
    fn new(value: &str, inspected_positions: &mut usize) -> Self {
        let mut next_label_or_whitespace = vec![value.len() as u32; value.len() + 1];
        let mut next = value.len();
        for (offset, character) in value.char_indices().rev() {
            *inspected_positions = inspected_positions.saturating_add(1);
            if character == '[' || character.is_whitespace() {
                next = offset;
            }
            next_label_or_whitespace[offset] =
                u32::try_from(next).expect("source length is bounded by TextSize");
        }
        Self {
            next_label_or_whitespace,
        }
    }

    fn has_label_before_whitespace(&self, value: &str, start: usize) -> bool {
        let next = self.next_label_or_whitespace(start);
        value.as_bytes().get(next) == Some(&b'[')
    }

    fn next_label_or_whitespace(&self, start: usize) -> usize {
        self.next_label_or_whitespace[start] as usize
    }

    #[cfg(test)]
    fn storage_bytes(&self) -> usize {
        self.next_label_or_whitespace.capacity() * std::mem::size_of::<u32>()
    }
}

fn url_link_candidate(value: &str, open: usize, index: &UrlCandidateIndex) -> bool {
    let candidate = &value[open..];
    let Some(scheme_end) = url_scheme_end(candidate) else {
        return false;
    };
    let remainder = &candidate[scheme_end..];
    remainder.starts_with("//")
        || starts_ascii_case_insensitive(candidate, "mailto:")
        // An explicit label marks an intentional link even for an opaque scheme.
        || index.has_label_before_whitespace(value, open + scheme_end)
}

fn macro_candidate(
    value: &str,
    open: usize,
    url_candidates: &UrlCandidateIndex,
    inspected_positions: &mut usize,
) -> bool {
    let candidate = &value[open..];
    if named_macro_candidate(candidate) {
        return true;
    }
    if email_candidate_start(value, open) {
        *inspected_positions = inspected_positions.saturating_add(email_scan_len(candidate));
        if email_address_end(candidate).is_some() {
            return true;
        }
    }
    if url_candidate_start(value, open) {
        *inspected_positions = inspected_positions.saturating_add(url_scan_len(candidate));
        return url_link_candidate(value, open, url_candidates);
    }
    false
}

fn email_scan_len(value: &str) -> usize {
    let local = value
        .bytes()
        .position(|byte| !email_local_part_byte(byte))
        .map_or(value.len(), |offset| offset + 1);
    if value.as_bytes().get(local.saturating_sub(1)) != Some(&b'@') {
        return local;
    }
    local
        + value[local..]
            .bytes()
            .position(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-')))
            .unwrap_or(value.len() - local)
}

fn url_scan_len(value: &str) -> usize {
    value
        .char_indices()
        .find_map(|(offset, character)| {
            (character == ':'
                || !character.is_ascii_alphanumeric()
                    && !matches!(character, '+' | '-' | '.' | '%'))
            .then_some(offset + character.len_utf8())
        })
        .unwrap_or(value.len())
}

fn named_macro_candidate(value: &str) -> bool {
    named_macro_prefix(value).is_some_and(NamedMacroPrefix::is_inline)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NamedMacroPrefix {
    Formula {
        prefix_len: usize,
    },
    Passthrough {
        prefix_len: usize,
    },
    Xref {
        prefix_len: usize,
    },
    Link {
        prefix_len: usize,
    },
    Standard {
        kind: StandardMacroKind,
        form: MacroForm,
        prefix_len: usize,
    },
}

impl NamedMacroPrefix {
    const fn is_inline(self) -> bool {
        !matches!(
            self,
            Self::Standard {
                form: MacroForm::Block,
                ..
            }
        )
    }
}

fn named_macro_prefix(value: &str) -> Option<NamedMacroPrefix> {
    if starts_ascii_case_insensitive(value, "stem:[") {
        Some(NamedMacroPrefix::Formula {
            prefix_len: "stem:[".len(),
        })
    } else if starts_ascii_case_insensitive(value, "latexmath:[") {
        Some(NamedMacroPrefix::Formula {
            prefix_len: "latexmath:[".len(),
        })
    } else if starts_ascii_case_insensitive(value, "pass:[") {
        Some(NamedMacroPrefix::Passthrough {
            prefix_len: "pass:[".len(),
        })
    } else if starts_ascii_case_insensitive(value, "xref:") {
        Some(NamedMacroPrefix::Xref {
            prefix_len: "xref:".len(),
        })
    } else if starts_ascii_case_insensitive(value, "link:") {
        Some(NamedMacroPrefix::Link {
            prefix_len: "link:".len(),
        })
    } else {
        standard_macro_prefix(value).map(|(kind, form, prefix_len)| NamedMacroPrefix::Standard {
            kind,
            form,
            prefix_len,
        })
    }
}

const fn email_local_part_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-')
}

fn email_candidate_start(value: &str, open: usize) -> bool {
    value.as_bytes()[open].is_ascii()
        && email_local_part_byte(value.as_bytes()[open])
        && open
            .checked_sub(1)
            .is_none_or(|previous| !email_local_part_byte(value.as_bytes()[previous]))
}

fn url_candidate_start(value: &str, open: usize) -> bool {
    value.as_bytes()[open].is_ascii_alphabetic()
        && open.checked_sub(1).is_none_or(|previous| {
            !matches!(
                value.as_bytes()[previous],
                b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'+' | b'-' | b'.' | b'%'
            )
        })
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
        && previous.is_none_or(|character| {
            !is_constrained_word_character(marker, character)
                && !(marker == '`' && matches!(character, ':' | ';' | '}'))
        })
}

fn is_close_boundary(value: &str, offset: usize, marker: char) -> bool {
    let previous = value[..offset].chars().next_back();
    let next = value[offset + marker.len_utf8()..].chars().next();
    previous.is_some_and(|character| !character.is_whitespace() && character != marker)
        && next.is_none_or(|character| !is_constrained_word_character(marker, character))
}

fn is_constrained_word_character(marker: char, character: char) -> bool {
    character.is_alphanumeric() || (marker == '`' && character == '_')
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
mod tests;
