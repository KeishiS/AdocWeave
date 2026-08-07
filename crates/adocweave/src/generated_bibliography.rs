//! Host-provided bibliography content appended by renderers as structured data.

/// A bibliography section generated from a library outside the document.
///
/// The title and entry contents are plain text. Renderers must never parse them
/// as AsciiDoc, expand attributes, or treat them as HTML.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GeneratedBibliography {
    title: String,
    entries: Vec<GeneratedBibliographyEntry>,
}

impl GeneratedBibliography {
    /// Creates a generated bibliography whose strings are all plain text.
    pub fn new(title: impl Into<String>, entries: Vec<GeneratedBibliographyEntry>) -> Self {
        Self {
            title: title.into(),
            entries,
        }
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    /// Entries in the order in which the renderer appends them.
    pub fn entries(&self) -> &[GeneratedBibliographyEntry] {
        &self.entries
    }
}

/// One entry in a host-generated bibliography section.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GeneratedBibliographyEntry {
    citation_key: String,
    text: String,
    label: Option<String>,
    number: Option<u32>,
}

impl GeneratedBibliographyEntry {
    /// Creates an entry whose body is rendered as plain text.
    pub fn new(citation_key: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            citation_key: citation_key.into(),
            text: text.into(),
            label: None,
            number: None,
        }
    }

    /// Sets the plain-text label shown for an unresolved citation.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Sets the position this entry takes in a numbered bibliography.
    ///
    /// Numbering is a property of the whole bibliography, not of one entry:
    /// either every entry carries a number or none does. The numbers that
    /// remain after invalid, duplicate and shadowed entries are dropped must
    /// read `1, 2, …, n` in the order the entries appear. A bibliography that
    /// breaks either rule is reported and not rendered at all, because a list
    /// whose numbers disagree with the citations in the body would send the
    /// reader to the wrong entry.
    #[must_use]
    pub const fn with_number(mut self, number: u32) -> Self {
        self.number = Some(number);
        self
    }

    pub fn citation_key(&self) -> &str {
        &self.citation_key
    }

    /// Plain-text bibliography body.
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// Position this entry takes in a numbered bibliography, if it has one.
    pub const fn number(&self) -> Option<u32> {
        self.number
    }
}
