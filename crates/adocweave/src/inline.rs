//! Inline scanner, recognizers, and semantic builders.

mod lowering;

use crate::budget::{BudgetExceeded, ParseBudget};
pub use crate::inline_model::*;
use crate::source::{TextRange, TextSize};

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
            | Self::TypographicQuote { open, .. }
            | Self::Passthrough { open, .. } => open,
        }
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
        let target_end = open + 3 + relative_end;
        return InlineRecognition::matched(
            value,
            InlineToken::Macro(MacroToken::ShorthandAnchor(ShorthandAnchorToken {
                kind: StandardMacroKind::BibliographyAnchor,
                open,
                target_start: open + 3,
                target_end,
                end: target_end + 3,
            })),
        );
    }
    if let Some(content) = rest.strip_prefix("[[")
        && let Some(relative_end) = content.find("]]")
    {
        let target_end = open + 2 + relative_end;
        return InlineRecognition::matched(
            value,
            InlineToken::Macro(MacroToken::ShorthandAnchor(ShorthandAnchorToken {
                kind: StandardMacroKind::Anchor,
                open,
                target_start: open + 2,
                target_end,
                end: target_end + 2,
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
mod tests {
    use super::{
        DelimiterIndex, FormulaToken, Inline, InlineCandidate, InlineCandidateIndex,
        InlineLiteralKind, InlineParseConfig, InlineProblemKind, InlineRecognition, InlineStyle,
        InlineToken, LinkToken, MacroForm, MacroToken, MarkerForm, MarkerToken,
        ReferenceDestination, ReferenceToken, StandardMacroKind, inline_at, next_candidate, parse,
        parse_text, recognize_macro, recognize_marker,
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
    fn candidate_index_has_fixed_linear_inspection_and_storage_budgets() {
        fn assert_bounded(source: &str) {
            let index = InlineCandidateIndex::new(source);
            assert!(index.inspected_positions() <= source.len().saturating_mul(12));
            assert!(
                index.storage_bytes() <= source.len().saturating_add(1).saturating_mul(128),
                "candidate index storage must remain linear"
            );
        }

        assert_eq!(InlineCandidateIndex::new("abc").inspected_positions(), 26);

        let source = "日本語 *open xref:broken[ https://example.org[label] _tail";
        let index = InlineCandidateIndex::new(source);

        assert!(index.inspected_positions() > source.len());
        assert_bounded(source);

        for repetitions in 1..128 {
            let hostile = "xref:".repeat(repetitions) + "target[open";
            assert_bounded(&hostile);
            let output = parse(
                &hostile,
                range(0, hostile.len()),
                InlineParseConfig::default(),
            );
            assert!(output.problems.len() <= 1);
        }
        for repetitions in 1..128 {
            let hostile = "\"`x ".repeat(repetitions);
            assert_bounded(&hostile);
        }
        let seed = include_str!("../../../fixtures/lint/macro-boundary-adversarial.adoc");
        let hostile = seed.repeat(256);
        assert_bounded(&hostile);
    }

    #[test]
    fn candidate_index_is_immutable_and_each_cursor_advances_independently() {
        let index = InlineCandidateIndex::new("*first* xref:target[]");
        let mut first = index.cursor();
        let mut second = index.cursor();

        assert_eq!(first.next(0), second.next(0));
        assert_eq!(first.next("*first* xref:target[]".len()), None);
        assert_eq!(
            second.next("*first* ".len()),
            Some(InlineCandidate::Macro {
                open: "*first* ".len()
            })
        );
    }

    #[test]
    fn scanner_delimiter_indexes_use_compact_offsets() {
        assert_eq!(std::mem::size_of::<u32>(), 4);
        let source = "[x] <<target>> ".repeat(1_024);
        let index = DelimiterIndex::new(&source);
        assert_eq!(index.storage_bytes(), (source.len() + 1) * 3 * 4);
        assert!(index.storage_bytes() <= source.len() * 13);
    }

    #[test]
    fn macro_recognizer_returns_ranges_without_building_nodes() {
        assert!(matches!(
            recognize_macro("stem:[x]", 0),
            InlineRecognition::Matched(InlineToken::Macro(MacroToken::Formula(FormulaToken {
                content_start: 6,
                content_end: 7,
                end: 8,
                closed: true,
                ..
            })))
        ));
        assert!(matches!(
            recognize_macro("<<id,label>>", 0),
            InlineRecognition::Matched(InlineToken::Macro(MacroToken::Reference(
                ReferenceToken::Short {
                    target_start: 2,
                    close: 10,
                    end: 12,
                    ..
                }
            )))
        ));
        assert!(matches!(
            recognize_macro("xref:other.adoc[Other]", 0),
            InlineRecognition::Matched(InlineToken::Macro(MacroToken::Reference(
                ReferenceToken::Xref {
                    target_start: 5,
                    bracket: 15,
                    close: 21,
                    end: 22,
                    ..
                }
            )))
        ));
        assert!(matches!(
            recognize_macro("https://example.org[label]", 0),
            InlineRecognition::Matched(InlineToken::Macro(MacroToken::Link(LinkToken::Url {
                target_end: 19,
                label: Some((20, 25)),
                end: 26,
                ..
            })))
        ));
        assert!(matches!(
            recognize_macro("image:asset.png[Alt]", 0),
            InlineRecognition::Matched(InlineToken::Macro(MacroToken::Standard(
                super::StandardMacroToken {
                    kind: StandardMacroKind::Image,
                    form: MacroForm::Inline,
                    target_start: 6,
                    bracket: 15,
                    close: 19,
                    end: 20,
                    ..
                }
            )))
        ));
        assert_eq!(
            recognize_macro("xref:other.adoc[open", 0),
            InlineRecognition::Recovered {
                open: 0,
                kind: InlineProblemKind::IncompleteCrossReference,
                next: 1,
            }
        );
        assert_eq!(
            recognize_macro("https://example.org[open", 0),
            InlineRecognition::Recovered {
                open: 0,
                kind: InlineProblemKind::IncompleteLink,
                next: 1,
            }
        );
    }

    #[test]
    fn marker_recognizer_distinguishes_complete_invalid_and_unclosed_input() {
        assert_eq!(
            recognize_marker("*strong*", 0, '*', MarkerForm::Constrained, Some(7),),
            InlineRecognition::Matched(InlineToken::Marker(MarkerToken {
                open: 0,
                close: 7,
                end: 8,
                marker: '*',
                form: MarkerForm::Constrained,
            }))
        );
        assert_eq!(
            recognize_marker("{bad name}", 0, '{', MarkerForm::Constrained, Some(9),),
            InlineRecognition::Rejected { open: 0, next: 1 }
        );
        assert_eq!(
            recognize_marker("_open", 0, '_', MarkerForm::Constrained, None),
            InlineRecognition::Recovered {
                open: 0,
                next: 1,
                kind: InlineProblemKind::UnclosedEmphasis,
            }
        );
    }

    #[test]
    fn selected_semantic_lowering_is_isolated_from_recognition() {
        const RECOGNITION: &str = include_str!("inline.rs");
        const LOWERING: &str = include_str!("inline/lowering.rs");

        for function in ["lower_marker", "lower_reference", "lower_standard_macro"] {
            assert!(LOWERING.contains(&format!("fn {function}(")));
            assert!(RECOGNITION.contains(&format!("lowering::{function}(")));
        }
        for constructor in ["Inline::Styled", "Inline::Reference", "Inline::Macro"] {
            assert!(LOWERING.contains(constructor));
        }
        for recognition_detail in [
            "fn recognize_",
            "InlineRecognition",
            "InlineCandidateIndex",
            "DelimiterIndex",
        ] {
            assert!(!LOWERING.contains(recognition_detail));
        }
        for old_builder in ["marker", "reference_macro", "standard_macro"] {
            assert!(!RECOGNITION.contains(&format!("fn build_{old_builder}(")));
        }
    }

    #[test]
    fn marker_reference_and_macro_lowering_preserve_utf8_ranges_deterministically() {
        fn source_slice<'a>(source: &'a str, base: usize, inline: &Inline) -> &'a str {
            let range = inline.range();
            let start = range.start().to_usize() - base;
            let end = range.end().to_usize() - base;
            assert!(start < end && end <= source.len());
            assert!(source.is_char_boundary(start));
            assert!(source.is_char_boundary(end));
            &source[start..end]
        }

        for fragment in ["a", "日本", "😀", "a-b_1", "é"] {
            let marker = format!("*{fragment}*");
            let reference = format!("xref:doc#{fragment}[_{fragment}_]");
            let macro_source = format!("image:{fragment}.png[Alt,{fragment}]");
            let source = format!("{marker} {reference} {macro_source}");
            let base = 7;
            let source_range = range(base, base + source.len());
            let first = parse(&source, source_range, InlineParseConfig::default());
            let second = parse(&source, source_range, InlineParseConfig::default());

            assert_eq!(first, second);
            assert!(first.problems.is_empty(), "{source:?}");
            assert_eq!(first.inlines.len(), 5);
            assert!(matches!(first.inlines[0], Inline::Styled { .. }));
            assert!(matches!(first.inlines[2], Inline::Reference(_)));
            assert!(matches!(first.inlines[4], Inline::Macro(_)));
            assert_eq!(source_slice(&source, base, &first.inlines[0]), marker);
            assert_eq!(source_slice(&source, base, &first.inlines[2]), reference);
            assert_eq!(source_slice(&source, base, &first.inlines[4]), macro_source);
            for inline in &first.inlines {
                let _ = source_slice(&source, base, inline);
            }
        }
    }

    #[test]
    fn candidate_recovery_always_advances_on_utf8_boundaries() {
        for source in [
            "日本語 xref:broken[ *open _also",
            "link:https://example.org[Label] image:asset.png[Alt]",
            "<<target,label>> https://example.org[label] user@example.org",
            "{bad name} **strong** stem:[x]",
        ] {
            let index = InlineCandidateIndex::new(source);
            let mut candidates = index.cursor();
            let mut cursor = 0;
            let mut steps = 0;
            while let Some(candidate) = candidates.next(cursor) {
                let recognition = index.recognize(source, candidate);
                let next = recognition.map_or_else(
                    || super::next_char_boundary(source, candidate.open()),
                    |recognition| {
                        assert!(recognition.is_well_formed(source));
                        assert_eq!(
                            Some(recognition),
                            index.recognize(source, candidate),
                            "recognition must be deterministic"
                        );
                        recognition.next()
                    },
                );
                assert!(next > cursor, "{source:?} at {cursor}");
                assert!(source.is_char_boundary(next));
                cursor = next;
                steps += 1;
            }
            assert!(steps <= source.chars().count());
        }
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
            references[0].authored_destination,
            ReferenceDestination::Local { ref anchor, .. } if anchor == "local"
        ));
        assert!(matches!(
            references[2].authored_destination,
            ReferenceDestination::Document { ref document, ref anchor, .. }
                if document == "other.adoc" && anchor.as_deref() == Some("part")
        ));
        assert!(matches!(
            references[3].authored_destination,
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
    fn constrained_monospace_rejects_standard_word_and_opening_boundaries() {
        for source in [
            "snake_`code`",
            "key:`code`",
            "key;`code`",
            "x}`code`",
            "日本`code`",
            "１`code`",
        ] {
            let output = parse(source, range(0, source.len()), InlineParseConfig::default());
            assert!(
                output
                    .inlines
                    .iter()
                    .all(|inline| matches!(inline, Inline::Text(_))),
                "{source:?} unexpectedly contained formatted inline content"
            );
            assert!(output.problems.is_empty(), "{source:?}");
        }

        let output = parse(
            "`code`_tail",
            range(0, "`code`_tail".len()),
            InlineParseConfig::default(),
        );
        assert!(
            output
                .inlines
                .iter()
                .all(|inline| matches!(inline, Inline::Text(_)))
        );
        assert_eq!(output.problems.len(), 2);
        assert_eq!(
            output
                .problems
                .iter()
                .map(|problem| (problem.kind, problem.range))
                .collect::<Vec<_>>(),
            [
                (InlineProblemKind::UnclosedMonospace, range(0, 1)),
                (InlineProblemKind::UnclosedEmphasis, range(6, 7)),
            ]
        );

        let output = parse(
            "`code`日本",
            range(0, "`code`日本".len()),
            InlineParseConfig::default(),
        );
        assert!(
            output
                .inlines
                .iter()
                .all(|inline| matches!(inline, Inline::Text(_)))
        );
        assert_eq!(output.problems.len(), 1);
        assert_eq!(
            (output.problems[0].kind, output.problems[0].range),
            (InlineProblemKind::UnclosedMonospace, range(0, 1))
        );
    }

    #[test]
    fn constrained_monospace_accepts_punctuation_and_unconstrained_ignores_boundaries() {
        let source =
            "key-`code` snake_``under`` key:``colon`` x}``brace`` 日本``和文``日本 😀``emoji``😀";
        let output = parse(source, range(0, source.len()), InlineParseConfig::default());
        let values = output
            .inlines
            .iter()
            .filter_map(|inline| match inline {
                Inline::Literal {
                    kind: InlineLiteralKind::Monospace,
                    value,
                    ..
                } => Some(value.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(values, ["code", "under", "colon", "brace", "和文", "emoji"]);
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
