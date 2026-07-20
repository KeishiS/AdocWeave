//! AdocWeave Language Server document state.

use std::collections::BTreeMap;

use adocweave::{Analysis, Engine, ParseOptions, SourceId};

#[derive(Debug)]
pub struct DocumentState {
    pub uri: String,
    pub version: i64,
    pub analysis: Analysis,
}

impl DocumentState {
    fn new(uri: String, version: i64, text: String) -> Result<Self, String> {
        let analysis = Engine::new(ParseOptions {
            source_id: Some(SourceId::new(uri.clone())),
            ..ParseOptions::default()
        })
        .analyze(&text)
        .map_err(|error| error.to_string())?;
        Ok(Self {
            uri,
            version,
            analysis,
        })
    }

    pub fn contains_line(&self, line: u32) -> bool {
        line < self.analysis.line_index.line_count()
    }
}

#[derive(Debug, Default)]
pub struct DocumentStore {
    documents: BTreeMap<String, DocumentState>,
}

impl DocumentStore {
    pub fn get(&self, uri: &str) -> Option<&DocumentState> {
        self.documents.get(uri)
    }

    pub fn open(&mut self, uri: String, version: i64, text: String) -> Result<(), String> {
        let state = DocumentState::new(uri.clone(), version, text)?;
        self.documents.insert(uri, state);
        Ok(())
    }

    pub fn change_full(&mut self, uri: &str, version: i64, text: String) -> Result<bool, String> {
        let Some(current) = self.documents.get(uri) else {
            return Ok(false);
        };
        if version <= current.version {
            return Ok(false);
        }
        let state = DocumentState::new(uri.to_owned(), version, text)?;
        self.documents.insert(uri.to_owned(), state);
        Ok(true)
    }

    pub fn close(&mut self, uri: &str) -> bool {
        self.documents.remove(uri).is_some()
    }

    pub fn len(&self) -> usize {
        self.documents.len()
    }

    pub fn is_empty(&self) -> bool {
        self.documents.is_empty()
    }
}
