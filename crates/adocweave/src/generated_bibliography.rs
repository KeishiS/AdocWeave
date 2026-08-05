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
}

impl GeneratedBibliographyEntry {
    /// Creates an entry whose body is rendered as plain text.
    pub fn new(citation_key: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            citation_key: citation_key.into(),
            text: text.into(),
            label: None,
        }
    }

    /// Sets the plain-text label shown for an unresolved citation.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
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
}
