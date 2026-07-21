//! Root-document header recognition and state.
//!
//! Nested block parsing never constructs this state. This keeps document
//! attributes, authors, and revision metadata out of recursive block output.

use crate::attributes::{AttributeProblem, DocumentAttribute};
use crate::parser::{Author, DocumentHeader, MetadataValue, Revision};
use crate::source::{PositionError, TextRange, TextSize};
use crate::source_document::SourceLine;

#[derive(Default)]
pub(super) struct DocumentHeaderState {
    pub(super) attributes_open: bool,
    pub(super) expect_author: bool,
    pub(super) expect_revision: bool,
    pub(super) header: DocumentHeader,
    pub(super) attributes: Vec<DocumentAttribute>,
    pub(super) attribute_problems: Vec<AttributeProblem>,
}

impl DocumentHeaderState {
    pub(super) fn close_attributes(&mut self) {
        self.attributes_open = false;
    }

    pub(super) fn stop_author_revision(&mut self) {
        self.expect_author = false;
        self.expect_revision = false;
    }

    pub(super) fn extend_range(&mut self, line: TextRange) {
        self.header.range = Some(match self.header.range {
            Some(range) => TextRange::new(range.start(), line.end()).expect("ordered header lines"),
            None => line,
        });
        self.header.end = line.end();
    }
}

pub(super) fn parse_author(
    content: &str,
    line: SourceLine,
) -> Result<Option<Author>, PositionError> {
    let trimmed = content.trim_matches([' ', '\t']);
    if trimmed.is_empty() {
        return Ok(None);
    }
    let leading = content.len() - content.trim_start_matches([' ', '\t']).len();
    let base = line.content_range().start().to_usize() + leading;
    let (name, email, email_offset) = match trimmed
        .strip_suffix('>')
        .and_then(|value| value.rsplit_once('<'))
    {
        Some((name, email)) if !email.trim().is_empty() => {
            let normalized_name = name.trim_end();
            (
                normalized_name,
                Some(email.trim()),
                Some(trimmed.len() - email.len() - 1),
            )
        }
        _ => (trimmed, None, None),
    };
    if name.is_empty() {
        return Ok(None);
    }
    let name_range = range(base, base + name.len())?;
    let email_range = email_offset
        .map(|offset| range(base + offset, base + offset + email.expect("email").len()))
        .transpose()?;
    Ok(Some(Author {
        range: line.full_range(),
        name_range,
        email_range,
        name: name.to_owned(),
        email: email.map(str::to_owned),
    }))
}

pub(super) fn parse_revision(content: &str, line: SourceLine) -> Result<Revision, PositionError> {
    let trimmed = content.trim_matches([' ', '\t']);
    let leading = content.len() - content.trim_start_matches([' ', '\t']).len();
    let base = line.content_range().start().to_usize() + leading;
    let (prefix, remark) = trimmed
        .split_once(':')
        .map_or((trimmed, None), |(left, right)| {
            (left.trim_end(), Some(right.trim_start()))
        });
    let (number, date) = prefix
        .split_once(',')
        .map_or((Some(prefix.trim()), None), |(left, right)| {
            (Some(left.trim()), Some(right.trim()))
        });
    let value = |part: Option<&str>| -> Result<Option<MetadataValue>, PositionError> {
        let Some(part) = part.filter(|part| !part.is_empty()) else {
            return Ok(None);
        };
        let offset = trimmed
            .find(part)
            .expect("revision component is a source slice");
        Ok(Some(MetadataValue {
            value: part.to_owned(),
            range: range(base + offset, base + offset + part.len())?,
        }))
    };
    Ok(Revision {
        range: line.full_range(),
        number: value(number)?,
        date: value(date)?,
        remark: value(remark)?,
    })
}

fn range(start: usize, end: usize) -> Result<TextRange, PositionError> {
    TextRange::new(TextSize::new(start)?, TextSize::new(end)?)
}
