//! Versioned document state and generation-checked analysis adoption.

use std::collections::BTreeMap;
use std::sync::Arc;

use adocweave::preprocessor::{AnalysisProjection, PreprocessedDocument};
use adocweave::source::TextRange;
use adocweave::{
    Analysis, AnalysisRequest, AnalysisResult, CancellationCheck, CancellationToken,
    DocumentRevision, ParseOptions, SourceId,
};

use crate::workspace::WorkspaceInput;

#[derive(Clone, Debug)]
pub struct AnalysisJob {
    pub uri: String,
    pub request: AnalysisRequest,
    pub cancellation: Arc<CancellationToken>,
    pub workspace: Option<WorkspaceInput>,
}

#[derive(Clone, Debug)]
pub struct DocumentState {
    pub uri: String,
    pub request: AnalysisRequest,
    pub analysis: Option<Arc<Analysis>>,
    pub workspace_analysis: Option<Arc<WorkspaceAnalysis>>,
    pub workspace_problem: Option<WorkspaceProblem>,
    cancellation: Arc<CancellationToken>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceProblem {
    pub source_id: Option<String>,
    pub range: TextRange,
    pub code: String,
    pub message: String,
}

#[derive(Debug)]
pub struct WorkspaceAnalysis {
    pub document: Arc<PreprocessedDocument>,
    pub analysis: Arc<Analysis>,
    pub projection: Arc<AnalysisProjection>,
    pub resource_versions: BTreeMap<String, i64>,
}

impl WorkspaceAnalysis {
    pub fn source_uris(&self) -> std::collections::BTreeSet<String> {
        let mut uris = std::collections::BTreeSet::new();
        for directive in &self.projection.directives {
            uris.extend(
                directive
                    .source_id
                    .iter()
                    .chain(directive.resource_source_id.iter())
                    .map(|source_id| source_id.as_str().to_owned()),
            );
        }
        for diagnostic in &self.projection.diagnostics {
            uris.extend(
                diagnostic
                    .origins
                    .iter()
                    .filter_map(|origin| origin.source_id.as_ref())
                    .map(|source_id| source_id.as_str().to_owned()),
            );
        }
        uris
    }
}

#[derive(Clone, Debug)]
pub struct DocumentSnapshot {
    pub uri: String,
    pub revision: DocumentRevision,
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
            revision: document.request.revision.clone(),
            analysis: document.analysis.clone()?,
        })
    }

    pub fn snapshots(&self) -> Vec<DocumentSnapshot> {
        self.documents
            .values()
            .filter_map(|document| self.snapshot(&document.uri))
            .collect()
    }

    pub fn workspace_analyses(&self) -> impl Iterator<Item = &WorkspaceAnalysis> {
        self.documents
            .values()
            .filter_map(|document| document.workspace_analysis.as_deref())
    }

    pub fn workspace_problems(&self) -> impl Iterator<Item = &WorkspaceProblem> {
        self.documents
            .values()
            .filter_map(|document| document.workspace_problem.as_ref())
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
                request: job.request.clone(),
                analysis: None,
                workspace_analysis: None,
                workspace_problem: None,
                cancellation: job.cancellation.clone(),
            },
        );
        job
    }

    pub fn begin_change(&mut self, uri: &str, version: i32, text: String) -> Option<AnalysisJob> {
        let current = self.documents.get(uri)?;
        if i64::from(version) <= current.request.revision.version {
            return None;
        }
        current.cancellation.cancel();
        let job = self.new_job(uri.to_owned(), version, text);
        let current = Arc::make_mut(&mut self.documents)
            .get_mut(uri)
            .expect("document existence checked");
        current.request = job.request.clone();
        current.analysis = None;
        current.workspace_analysis = None;
        current.workspace_problem = None;
        current.cancellation = job.cancellation.clone();
        Some(job)
    }

    pub fn begin_reanalysis(&mut self, uri: &str) -> Option<AnalysisJob> {
        let current = self.documents.get(uri)?;
        current.cancellation.cancel();
        let version = i32::try_from(current.request.revision.version).ok()?;
        let text = current.request.source.to_string();
        let job = self.new_job(uri.to_owned(), version, text);
        let current = Arc::make_mut(&mut self.documents)
            .get_mut(uri)
            .expect("document existence checked");
        current.request = job.request.clone();
        current.analysis = None;
        current.workspace_analysis = None;
        current.workspace_problem = None;
        current.cancellation = job.cancellation.clone();
        Some(job)
    }

    pub fn adopt(&mut self, job: &AnalysisJob, result: AnalysisResult) -> Adoption {
        let Some(document) = self.documents.get(&job.uri) else {
            return Adoption::Closed;
        };
        if !result.is_current(&document.request.revision, job.cancellation.as_ref()) {
            return Adoption::Stale;
        }
        let document = Arc::make_mut(&mut self.documents)
            .get_mut(&job.uri)
            .expect("document existence checked");
        document.analysis = Some(Arc::new(result.analysis));
        Adoption::Adopted
    }

    pub fn adopt_workspace(&mut self, job: &AnalysisJob, analysis: WorkspaceAnalysis) -> Adoption {
        let Some(document) = self.documents.get(&job.uri) else {
            return Adoption::Closed;
        };
        if document.request.revision != job.request.revision || job.cancellation.is_cancelled() {
            return Adoption::Stale;
        }
        Arc::make_mut(&mut self.documents)
            .get_mut(&job.uri)
            .expect("document existence checked")
            .workspace_analysis = Some(Arc::new(analysis));
        Arc::make_mut(&mut self.documents)
            .get_mut(&job.uri)
            .expect("document existence checked")
            .workspace_problem = None;
        Adoption::Adopted
    }

    pub fn adopt_workspace_problem(
        &mut self,
        job: &AnalysisJob,
        problem: WorkspaceProblem,
    ) -> Adoption {
        let Some(document) = self.documents.get(&job.uri) else {
            return Adoption::Closed;
        };
        if document.request.revision != job.request.revision || job.cancellation.is_cancelled() {
            return Adoption::Stale;
        }
        let document = Arc::make_mut(&mut self.documents)
            .get_mut(&job.uri)
            .expect("document existence checked");
        document.workspace_analysis = None;
        document.workspace_problem = Some(problem);
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
        let request = AnalysisRequest::new(
            Some(SourceId::new(uri.clone())),
            i64::from(version),
            self.next_generation,
            text,
            ParseOptions::default(),
        );
        AnalysisJob {
            uri,
            request,
            cancellation: Arc::new(CancellationToken::new()),
            workspace: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use adocweave::{CancellationCheck, NeverCancel};

    use super::{Adoption, AnalysisJob, DocumentStore};

    fn analyze(job: &AnalysisJob) -> adocweave::AnalysisResult {
        job.request.analyze(&NeverCancel).expect("analysis")
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
        assert_eq!(snapshot.revision.version, 2);
        assert_eq!(
            snapshot.revision.generation,
            new.request.revision.generation
        );
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
