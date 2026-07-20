//! Versioned document state and generation-checked analysis adoption.

use std::collections::BTreeMap;
use std::sync::Arc;

use adocweave::{Analysis, CancellationToken};

#[derive(Clone, Debug)]
pub struct AnalysisJob {
    pub uri: String,
    pub version: i32,
    pub generation: u64,
    pub source: Arc<str>,
    pub cancellation: Arc<CancellationToken>,
}

#[derive(Clone, Debug)]
pub struct DocumentState {
    pub uri: String,
    pub version: i32,
    pub generation: u64,
    pub source: Arc<str>,
    pub analysis: Option<Arc<Analysis>>,
    cancellation: Arc<CancellationToken>,
}

#[derive(Clone, Debug)]
pub struct DocumentSnapshot {
    pub uri: String,
    pub version: i32,
    pub generation: u64,
    pub analysis: Arc<Analysis>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Adoption {
    Adopted,
    Stale,
    Closed,
}

#[derive(Clone, Debug, Default)]
pub struct DocumentStore {
    documents: Arc<BTreeMap<String, DocumentState>>,
    next_generation: u64,
}

impl DocumentStore {
    pub fn get(&self, uri: &str) -> Option<&DocumentState> {
        self.documents.get(uri)
    }

    pub fn snapshot(&self, uri: &str) -> Option<DocumentSnapshot> {
        let document = self.documents.get(uri)?;
        Some(DocumentSnapshot {
            uri: document.uri.clone(),
            version: document.version,
            generation: document.generation,
            analysis: document.analysis.clone()?,
        })
    }

    pub fn snapshots(&self) -> Vec<DocumentSnapshot> {
        self.documents
            .values()
            .filter_map(|document| self.snapshot(&document.uri))
            .collect()
    }

    pub fn cancellation(&self, uri: &str) -> Option<Arc<CancellationToken>> {
        self.documents
            .get(uri)
            .map(|document| document.cancellation.clone())
    }

    pub fn begin_open(&mut self, uri: String, version: i32, text: String) -> AnalysisJob {
        if let Some(previous) = self.documents.get(&uri) {
            previous.cancellation.cancel();
        }
        let job = self.new_job(uri.clone(), version, text);
        Arc::make_mut(&mut self.documents).insert(
            uri.clone(),
            DocumentState {
                uri,
                version,
                generation: job.generation,
                source: job.source.clone(),
                analysis: None,
                cancellation: job.cancellation.clone(),
            },
        );
        job
    }

    pub fn begin_change(&mut self, uri: &str, version: i32, text: String) -> Option<AnalysisJob> {
        let current = self.documents.get(uri)?;
        if version <= current.version {
            return None;
        }
        current.cancellation.cancel();
        let job = self.new_job(uri.to_owned(), version, text);
        let current = Arc::make_mut(&mut self.documents)
            .get_mut(uri)
            .expect("document existence checked");
        current.version = version;
        current.generation = job.generation;
        current.source = job.source.clone();
        current.analysis = None;
        current.cancellation = job.cancellation.clone();
        Some(job)
    }

    pub fn adopt(&mut self, job: &AnalysisJob, analysis: Analysis) -> Adoption {
        let Some(document) = self.documents.get(&job.uri) else {
            return Adoption::Closed;
        };
        if document.generation != job.generation || document.version != job.version {
            return Adoption::Stale;
        }
        let document = Arc::make_mut(&mut self.documents)
            .get_mut(&job.uri)
            .expect("document existence checked");
        document.analysis = Some(Arc::new(analysis));
        Adoption::Adopted
    }

    pub fn close(&mut self, uri: &str) -> bool {
        if !self.documents.contains_key(uri) {
            return false;
        }
        let document = Arc::make_mut(&mut self.documents)
            .remove(uri)
            .expect("document existence checked");
        document.cancellation.cancel();
        true
    }

    pub fn cancel_all(&mut self) {
        for document in self.documents.values() {
            document.cancellation.cancel();
        }
    }

    fn new_job(&mut self, uri: String, version: i32, text: String) -> AnalysisJob {
        self.next_generation = self.next_generation.saturating_add(1);
        AnalysisJob {
            uri,
            version,
            generation: self.next_generation,
            source: Arc::from(text),
            cancellation: Arc::new(CancellationToken::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use adocweave::{CancellationCheck, Engine, NeverCancel, ParseOptions, SourceId};

    use super::{Adoption, AnalysisJob, DocumentStore};

    fn analyze(job: &AnalysisJob) -> adocweave::Analysis {
        Engine::new(ParseOptions {
            source_id: Some(SourceId::new(job.uri.clone())),
            ..ParseOptions::default()
        })
        .analyze_cancellable(&job.source, &NeverCancel)
        .expect("analysis")
    }

    #[test]
    fn notification_order_newer_generation_cancels_and_rejects_previous_analysis() {
        let mut store = DocumentStore::default();
        let old = store.begin_open("file:///a.adoc".to_owned(), 1, "= Old".to_owned());
        let new = store
            .begin_change("file:///a.adoc", 2, "= New".to_owned())
            .expect("new generation");

        assert!(old.cancellation.is_cancelled());
        assert!(!new.cancellation.is_cancelled());
        assert_eq!(store.adopt(&old, analyze(&old)), Adoption::Stale);
        assert_eq!(store.adopt(&new, analyze(&new)), Adoption::Adopted);
        let snapshot = store.snapshot("file:///a.adoc").expect("snapshot");
        assert_eq!(snapshot.version, 2);
        assert_eq!(snapshot.generation, new.generation);
        assert_eq!(snapshot.analysis.source(), "= New");
    }

    #[test]
    fn pending_and_closed_documents_never_expose_an_analysis_snapshot() {
        let mut store = DocumentStore::default();
        let job = store.begin_open("file:///a.adoc".to_owned(), 1, "= A".to_owned());
        assert!(store.snapshot("file:///a.adoc").is_none());

        assert!(store.close("file:///a.adoc"));
        assert!(job.cancellation.is_cancelled());
        assert_eq!(store.adopt(&job, analyze(&job)), Adoption::Closed);
        assert!(store.snapshot("file:///a.adoc").is_none());
    }

    #[test]
    fn shutdown_cancels_every_open_document() {
        let mut store = DocumentStore::default();
        let first = store.begin_open("file:///a.adoc".to_owned(), 1, "= A".to_owned());
        let second = store.begin_open("file:///b.adoc".to_owned(), 1, "= B".to_owned());

        store.cancel_all();

        assert!(first.cancellation.is_cancelled());
        assert!(second.cancellation.is_cancelled());
    }

    #[test]
    fn cloned_store_is_an_owned_copy_on_write_snapshot() {
        let mut store = DocumentStore::default();
        let first = store.begin_open("file:///a.adoc".to_owned(), 1, "= Old".to_owned());
        assert_eq!(store.adopt(&first, analyze(&first)), Adoption::Adopted);
        let snapshot = store.clone();

        let second = store
            .begin_change("file:///a.adoc", 2, "= New".to_owned())
            .expect("new generation");
        assert_eq!(store.adopt(&second, analyze(&second)), Adoption::Adopted);

        assert_eq!(
            snapshot
                .snapshot("file:///a.adoc")
                .expect("old snapshot")
                .analysis
                .source(),
            "= Old"
        );
        assert_eq!(
            store
                .snapshot("file:///a.adoc")
                .expect("new snapshot")
                .analysis
                .source(),
            "= New"
        );
    }
}
