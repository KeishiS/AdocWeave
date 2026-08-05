//! Workspace scan coordination, recovery, and watched-change replay.

use std::collections::BTreeMap;
use std::sync::Arc;

use adocweave::CancellationToken;
use async_lsp::lsp_types::{DidChangeWatchedFilesParams, FileChangeType, FileEvent, Url};

use crate::service::{LanguageService, WorkspaceFileChanges};
use crate::state::AnalysisJob;

const MAX_WATCH_JOURNAL_ENTRIES: usize = 10_000;
const MAX_WATCH_JOURNAL_URI_BYTES: usize = 1024 * 1024;

#[derive(Default)]
pub(super) struct WorkspaceScanRecoveryTimer {
    state: WorkspaceScanRecoveryTimerState,
}

#[derive(Default)]
enum WorkspaceScanRecoveryTimerState {
    #[default]
    Idle,
    Debouncing {
        generation: u64,
        task: AbortOnDrop,
    },
}

struct AbortOnDrop {
    handle: tokio::task::JoinHandle<()>,
    abort: bool,
}

impl AbortOnDrop {
    fn new(handle: tokio::task::JoinHandle<()>) -> Self {
        Self {
            handle,
            abort: true,
        }
    }

    fn completed(mut self) {
        self.abort = false;
    }
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if self.abort {
            self.handle.abort();
        }
    }
}

impl WorkspaceScanRecoveryTimer {
    pub(super) fn replace(&mut self, generation: u64, handle: tokio::task::JoinHandle<()>) {
        self.cancel();
        self.state = WorkspaceScanRecoveryTimerState::Debouncing {
            generation,
            task: AbortOnDrop::new(handle),
        };
    }

    pub(super) fn complete(&mut self, generation: u64) -> bool {
        if !matches!(&self.state, WorkspaceScanRecoveryTimerState::Debouncing { generation: current, .. } if *current == generation)
        {
            return false;
        }
        let WorkspaceScanRecoveryTimerState::Debouncing { task, .. } =
            std::mem::take(&mut self.state)
        else {
            unreachable!("matching debounce state was checked above");
        };
        task.completed();
        true
    }

    pub(super) fn cancel(&mut self) {
        self.state = WorkspaceScanRecoveryTimerState::Idle;
    }

    #[cfg(test)]
    fn generation(&self) -> Option<u64> {
        match &self.state {
            WorkspaceScanRecoveryTimerState::Idle => None,
            WorkspaceScanRecoveryTimerState::Debouncing { generation, .. } => Some(*generation),
        }
    }
}

#[derive(Default)]
enum WorkspaceScanPhase {
    #[default]
    Idle,
    Running(ActiveWorkspaceScan),
}

struct ActiveWorkspaceScan {
    sequence: u64,
    cancellation: Arc<CancellationToken>,
    accept_result: bool,
    rejection: Option<String>,
}

pub(super) struct WorkspaceScanStart {
    sequence: u64,
    cancellation: Arc<CancellationToken>,
}

impl WorkspaceScanStart {
    pub(super) fn into_parts(self) -> (u64, Arc<CancellationToken>) {
        (self.sequence, self.cancellation)
    }
}

struct WorkspaceScanCompletion {
    accept_result: bool,
    rejection: Option<String>,
    next: Option<WorkspaceScanStart>,
}

#[derive(Default)]
enum WorkspaceRecoveryState {
    #[default]
    Idle,
    /// A timer handle exists in `Backend` for this generation.
    Debouncing {
        generation: u64,
        minimum_scan_sequence: u64,
    },
    /// The timer fired, but the active scan and its replay journal can satisfy
    /// the recovery. No timer handle exists while completion is awaited.
    AwaitingActiveCompletion {
        generation: u64,
        minimum_scan_sequence: u64,
    },
}

#[derive(Default)]
pub(super) struct WorkspaceScanCoordinator {
    sequence: u64,
    phase: WorkspaceScanPhase,
    pending_replacement: bool,
    watched_changes: WatchedChangeJournal,
    recovery_generation: u64,
    recovery: WorkspaceRecoveryState,
}

impl WorkspaceScanCoordinator {
    pub(super) fn request_replacement(&mut self) -> Option<WorkspaceScanStart> {
        self.disarm_recovery();
        self.watched_changes.clear();
        if let WorkspaceScanPhase::Running(active) = &mut self.phase {
            active.accept_result = false;
            active.rejection = None;
            active.cancellation.cancel();
            self.pending_replacement = true;
            return None;
        }
        Some(self.start())
    }

    fn reject_result(&mut self, reason: String) {
        if let WorkspaceScanPhase::Running(active) = &mut self.phase {
            active.accept_result = false;
            active.rejection.get_or_insert(reason);
        }
    }

    fn reject_unreplayable_watch(&mut self) {
        if let WorkspaceScanPhase::Running(active) = &mut self.phase {
            active.accept_result = false;
            active.rejection = None;
        }
    }

    fn accepts_active_result(&self) -> bool {
        matches!(&self.phase, WorkspaceScanPhase::Running(active) if active.accept_result)
    }

    fn complete_active(&mut self, sequence: u64) -> Option<WorkspaceScanCompletion> {
        let WorkspaceScanPhase::Running(active) =
            std::mem::replace(&mut self.phase, WorkspaceScanPhase::Idle)
        else {
            return None;
        };
        if active.sequence != sequence {
            self.phase = WorkspaceScanPhase::Running(active);
            return None;
        }
        let start_next = std::mem::take(&mut self.pending_replacement);
        let next = start_next.then(|| self.start());
        Some(WorkspaceScanCompletion {
            accept_result: active.accept_result,
            rejection: active.rejection,
            next,
        })
    }

    pub(super) fn cancel(&mut self) {
        if let WorkspaceScanPhase::Running(active) = &mut self.phase {
            active.accept_result = false;
            active.rejection = None;
            active.cancellation.cancel();
        }
        self.pending_replacement = false;
        self.watched_changes.clear();
        self.disarm_recovery();
    }

    fn start(&mut self) -> WorkspaceScanStart {
        debug_assert!(matches!(self.phase, WorkspaceScanPhase::Idle));
        self.sequence = self.sequence.saturating_add(1);
        let cancellation = Arc::new(CancellationToken::new());
        self.phase = WorkspaceScanPhase::Running(ActiveWorkspaceScan {
            sequence: self.sequence,
            cancellation: Arc::clone(&cancellation),
            accept_result: true,
            rejection: None,
        });
        WorkspaceScanStart {
            sequence: self.sequence,
            cancellation,
        }
    }

    fn arm_recovery(&mut self, minimum_scan_sequence: u64) -> u64 {
        let minimum_scan_sequence = match self.recovery {
            WorkspaceRecoveryState::Idle => minimum_scan_sequence,
            WorkspaceRecoveryState::Debouncing {
                minimum_scan_sequence: existing,
                ..
            }
            | WorkspaceRecoveryState::AwaitingActiveCompletion {
                minimum_scan_sequence: existing,
                ..
            } => existing.max(minimum_scan_sequence),
        };
        self.recovery_generation = self.recovery_generation.saturating_add(1);
        let generation = self.recovery_generation;
        self.recovery = WorkspaceRecoveryState::Debouncing {
            generation,
            minimum_scan_sequence,
        };
        generation
    }

    fn disarm_recovery(&mut self) {
        self.recovery_generation = self.recovery_generation.saturating_add(1);
        self.recovery = WorkspaceRecoveryState::Idle;
    }

    fn recovery_generation(&self) -> Option<u64> {
        match self.recovery {
            WorkspaceRecoveryState::Idle => None,
            WorkspaceRecoveryState::Debouncing { generation, .. }
            | WorkspaceRecoveryState::AwaitingActiveCompletion { generation, .. } => {
                Some(generation)
            }
        }
    }

    pub(super) fn debouncing_generation(&self) -> Option<u64> {
        match self.recovery {
            WorkspaceRecoveryState::Debouncing { generation, .. } => Some(generation),
            WorkspaceRecoveryState::Idle
            | WorkspaceRecoveryState::AwaitingActiveCompletion { .. } => None,
        }
    }

    fn recovery_minimum_scan_sequence(&self) -> Option<u64> {
        match self.recovery {
            WorkspaceRecoveryState::Idle => None,
            WorkspaceRecoveryState::Debouncing {
                minimum_scan_sequence,
                ..
            }
            | WorkspaceRecoveryState::AwaitingActiveCompletion {
                minimum_scan_sequence,
                ..
            } => Some(minimum_scan_sequence),
        }
    }

    fn active_or_next_sequence(&self) -> u64 {
        match &self.phase {
            WorkspaceScanPhase::Idle => self.sequence.saturating_add(1),
            WorkspaceScanPhase::Running(active) => active.sequence,
        }
    }

    fn sequence_after_active(&self) -> u64 {
        match &self.phase {
            WorkspaceScanPhase::Idle => self.sequence.saturating_add(1),
            WorkspaceScanPhase::Running(active) => active.sequence.saturating_add(1),
        }
    }

    fn rearm_recovery(&mut self) -> Option<u64> {
        let minimum = self.recovery_minimum_scan_sequence()?;
        Some(self.arm_recovery(minimum))
    }

    fn disarm_recovery_if_covered(&mut self, sequence: u64) -> bool {
        if self
            .recovery_minimum_scan_sequence()
            .is_some_and(|minimum| sequence < minimum)
        {
            return false;
        }
        self.disarm_recovery();
        true
    }
}

#[derive(Default)]
struct WatchedChangeJournal {
    changes: BTreeMap<Url, FileChangeType>,
    uri_bytes: usize,
    overflowed: bool,
}

impl WatchedChangeJournal {
    fn record(&mut self, changes: &[FileEvent]) -> bool {
        self.record_with_limits(
            changes,
            MAX_WATCH_JOURNAL_ENTRIES,
            MAX_WATCH_JOURNAL_URI_BYTES,
        )
    }

    fn record_with_limits(
        &mut self,
        changes: &[FileEvent],
        max_entries: usize,
        max_uri_bytes: usize,
    ) -> bool {
        if self.overflowed {
            return false;
        }
        for change in changes {
            let is_new = !self.changes.contains_key(&change.uri);
            let additional_bytes = if is_new { change.uri.as_str().len() } else { 0 };
            if self.changes.len().saturating_add(usize::from(is_new)) > max_entries
                || self.uri_bytes.saturating_add(additional_bytes) > max_uri_bytes
            {
                self.changes.clear();
                self.uri_bytes = 0;
                self.overflowed = true;
                return false;
            }
            self.uri_bytes = self.uri_bytes.saturating_add(additional_bytes);
            self.changes.insert(change.uri.clone(), change.typ);
        }
        true
    }

    fn take(&mut self) -> Option<DidChangeWatchedFilesParams> {
        if self.overflowed {
            self.clear();
            return None;
        }
        let changes = std::mem::take(&mut self.changes)
            .into_iter()
            .map(|(uri, typ)| FileEvent { uri, typ })
            .collect::<Vec<_>>();
        self.uri_bytes = 0;
        (!changes.is_empty()).then_some(DidChangeWatchedFilesParams { changes })
    }

    fn clear(&mut self) {
        self.changes.clear();
        self.uri_bytes = 0;
        self.overflowed = false;
    }
}

impl WorkspaceScanCoordinator {
    pub(super) fn record_workspace_changes(
        &mut self,
        changes: &WorkspaceFileChanges,
    ) -> Option<u64> {
        let mut recovery_generation = self.record_watched_changes(&changes.journal);
        if changes.recovery_required {
            recovery_generation = Some(if changes.replay_complete {
                self.request_quiet_recovery()
            } else {
                self.request_unreplayable_recovery()
            });
        }
        recovery_generation
    }

    fn record_watched_changes(&mut self, changes: &[FileEvent]) -> Option<u64> {
        if self.accepts_active_result() && !self.watched_changes.record(changes) {
            // The journal can no longer reconstruct all changes made after the
            // worker took its snapshot. Keep the incrementally updated service
            // state and reject that snapshot instead of installing older
            // contents over it. The worker is allowed to finish and reports a
            // bounded failure instead of retrying forever under a notification
            // stream that exceeds this safety limit.
            self.reject_result(format!(
                "workspace watch journal limit exceeded: at most {MAX_WATCH_JOURNAL_ENTRIES} entries and {MAX_WATCH_JOURNAL_URI_BYTES} URI bytes may change during one scan"
            ));
            let minimum = self.sequence_after_active();
            return Some(self.arm_recovery(minimum));
        }
        if self.recovery_generation().is_some() && !changes.is_empty() {
            self.rearm_recovery()
        } else {
            None
        }
    }

    pub(super) fn request_recovery(&mut self, generation: u64) -> Option<WorkspaceScanStart> {
        let minimum = match self.recovery {
            WorkspaceRecoveryState::Debouncing {
                generation: current,
                minimum_scan_sequence,
            } if current == generation => minimum_scan_sequence,
            WorkspaceRecoveryState::Idle
            | WorkspaceRecoveryState::Debouncing { .. }
            | WorkspaceRecoveryState::AwaitingActiveCompletion { .. } => return None,
        };
        if let WorkspaceScanPhase::Running(active) = &self.phase
            && active.accept_result
            && active.sequence >= minimum
        {
            // This worker can still produce a snapshot that contains the
            // recovery lineage. Keep both its replay journal and the recovery
            // reservation until completion proves that installation and replay
            // succeeded.
            self.recovery = WorkspaceRecoveryState::AwaitingActiveCompletion {
                generation,
                minimum_scan_sequence: minimum,
            };
            return None;
        }
        self.recovery = WorkspaceRecoveryState::Idle;
        self.watched_changes.clear();
        if let WorkspaceScanPhase::Running(active) = &mut self.phase {
            // Discarding the journal makes this worker's snapshot impossible to
            // reconcile, even if it was independently acceptable before the
            // recovery timer fired.
            active.accept_result = false;
            self.pending_replacement = true;
            None
        } else {
            Some(self.start())
        }
    }

    fn request_quiet_recovery(&mut self) -> u64 {
        self.arm_recovery(self.active_or_next_sequence())
    }

    fn request_unreplayable_recovery(&mut self) -> u64 {
        let minimum = self.sequence_after_active();
        self.reject_unreplayable_watch();
        self.arm_recovery(minimum)
    }

    pub(super) fn complete(
        &mut self,
        service: &mut LanguageService,
        scanned: WorkspaceScanned,
    ) -> Option<WorkspaceScanTransition> {
        let completion = self.complete_active(scanned.sequence)?;
        let mut jobs = Vec::new();
        let mut recovery_timer = WorkspaceRecoveryTimerUpdate::Keep;
        if completion.accept_result {
            match scanned.scan {
                Ok(scan) => {
                    let application = service.apply_workspace_scan(scan);
                    jobs.extend(application.jobs);
                    let mut replay_requires_recovery = false;
                    if let Some(changes) = self.watched_changes.take() {
                        let replay = service.workspace_files_changed_with_journal(changes);
                        jobs.extend(replay.jobs);
                        if replay.recovery_required {
                            recovery_timer = WorkspaceRecoveryTimerUpdate::Arm(
                                self.arm_recovery(scanned.sequence.saturating_add(1)),
                            );
                            replay_requires_recovery = true;
                        }
                    }
                    if application.installed
                        && !replay_requires_recovery
                        && self.disarm_recovery_if_covered(scanned.sequence)
                    {
                        recovery_timer = WorkspaceRecoveryTimerUpdate::Cancel;
                    }
                }
                Err(error) => {
                    self.watched_changes.clear();
                    jobs.extend(service.workspace_scan_failed(error));
                }
            }
        } else {
            self.watched_changes.clear();
            if let Some(error) = completion.rejection {
                jobs.extend(service.workspace_scan_failed(error));
            }
        }
        Some(WorkspaceScanTransition {
            jobs,
            next: completion.next,
            recovery_timer,
        })
    }
}

/// A workspace read that finished on a worker and is ready to install.
///
/// `sequence` identifies the single active worker. Cancelled results still emit
/// this event so the main loop can start one coalesced replacement without
/// allowing scan workers to overlap.
pub(super) struct WorkspaceScanned {
    sequence: u64,
    scan: Result<crate::service::WorkspaceScan, String>,
}

impl WorkspaceScanned {
    pub(super) fn new(sequence: u64, scan: Result<crate::service::WorkspaceScan, String>) -> Self {
        Self { sequence, scan }
    }
}

pub(super) struct WorkspaceScanRecovery {
    generation: u64,
}

impl WorkspaceScanRecovery {
    pub(super) fn new(generation: u64) -> Self {
        Self { generation }
    }

    pub(super) fn generation(&self) -> u64 {
        self.generation
    }
}

pub(super) struct WorkspaceScanTransition {
    jobs: Vec<AnalysisJob>,
    next: Option<WorkspaceScanStart>,
    recovery_timer: WorkspaceRecoveryTimerUpdate,
}

impl WorkspaceScanTransition {
    pub(super) fn into_parts(
        self,
    ) -> (
        Vec<AnalysisJob>,
        Option<WorkspaceScanStart>,
        WorkspaceRecoveryTimerUpdate,
    ) {
        (self.jobs, self.next, self.recovery_timer)
    }
}

pub(super) enum WorkspaceRecoveryTimerUpdate {
    Keep,
    Cancel,
    Arm(u64),
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::mpsc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use adocweave::CancellationCheck;
    use serde_json::json;

    use super::*;

    struct NotifyOnDrop(mpsc::Sender<()>);

    impl Drop for NotifyOnDrop {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }

    fn scan_race_service(prefix: &str) -> (std::path::PathBuf, Url, LanguageService) {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("{prefix}-{unique}"));
        fs::create_dir_all(&root).expect("workspace");
        let document_path = root.join("root.adoc");
        fs::write(&document_path, "= Before\n").expect("initial document");
        let root_uri = Url::from_directory_path(&root).expect("root URI");
        let document_uri = Url::from_file_path(&document_path).expect("document URI");
        let params = serde_json::from_value(json!({
            "processId": null,
            "rootUri": root_uri,
            "capabilities": {}
        }))
        .expect("initialize params");
        let mut service = LanguageService::default();
        service.initialize(&params);
        let scan = service.plan_workspace_scan(&adocweave::NeverCancel);
        let _ = service.apply_workspace_scan(scan);
        (root, document_uri, service)
    }

    #[test]
    fn replacement_scans_are_coalesced_without_overlapping_workers() {
        let mut coordinator = WorkspaceScanCoordinator::default();
        let old = coordinator.request_replacement().expect("initial scan");

        for _ in 0..100 {
            assert!(coordinator.request_replacement().is_none());
        }

        assert!(old.cancellation.is_cancelled());
        assert!(!coordinator.accepts_active_result());
        let completion = coordinator
            .complete_active(old.sequence)
            .expect("old completion");
        assert!(!completion.accept_result);
        let new = completion.next.expect("one replacement");
        assert!(!new.cancellation.is_cancelled());

        let completion = coordinator
            .complete_active(new.sequence)
            .expect("new completion");
        assert!(completion.accept_result);
        assert!(completion.next.is_none());
    }

    #[test]
    fn shutdown_cancels_the_active_scan_and_discards_pending_work() {
        let mut coordinator = WorkspaceScanCoordinator::default();
        let active = coordinator.request_replacement().expect("initial scan");
        assert!(coordinator.request_replacement().is_none());

        coordinator.cancel();

        assert!(active.cancellation.is_cancelled());
        let completion = coordinator
            .complete_active(active.sequence)
            .expect("completion");
        assert!(!completion.accept_result);
        assert!(completion.next.is_none());
    }

    #[test]
    fn stale_scan_completion_cannot_replace_the_active_scan() {
        let mut coordinator = WorkspaceScanCoordinator::default();
        let active = coordinator.request_replacement().expect("initial scan");

        assert!(coordinator.complete_active(active.sequence + 1).is_none());
        assert!(coordinator.accepts_active_result());
        assert!(
            coordinator
                .complete_active(active.sequence)
                .expect("active completion")
                .accept_result
        );
    }

    #[test]
    fn continuous_watched_changes_do_not_cancel_or_restart_the_active_scan() {
        let mut coordinator = WorkspaceScanCoordinator::default();
        let active = coordinator.request_replacement().expect("initial scan");
        let uri = Url::parse("file:///workspace/root.adoc").expect("URI");

        for _ in 0..100 {
            assert!(
                coordinator
                    .record_watched_changes(&[
                        FileEvent::new(uri.clone(), FileChangeType::CHANGED,)
                    ])
                    .is_none()
            );
        }

        assert!(!active.cancellation.is_cancelled());
        assert_eq!(coordinator.watched_changes.changes.len(), 1);
        let completion = coordinator
            .complete_active(active.sequence)
            .expect("scan completion");
        assert!(completion.accept_result);
        assert!(completion.next.is_none());
    }

    #[test]
    fn structural_replacement_discards_watch_state_and_stale_recovery() {
        let mut coordinator = WorkspaceScanCoordinator::default();
        let active = coordinator.request_replacement().expect("initial scan");
        let uri = Url::parse("file:///workspace/root.adoc").expect("URI");
        let _ = coordinator.record_watched_changes(&[FileEvent::new(uri, FileChangeType::CHANGED)]);
        let stale_recovery = coordinator.request_quiet_recovery();

        assert!(coordinator.request_replacement().is_none());
        assert!(active.cancellation.is_cancelled());
        assert!(coordinator.watched_changes.changes.is_empty());
        assert!(coordinator.recovery_generation().is_none());
        assert!(coordinator.request_recovery(stale_recovery).is_none());

        let completion = coordinator
            .complete_active(active.sequence)
            .expect("cancelled completion");
        assert!(!completion.accept_result);
        assert!(completion.next.is_some());
    }

    #[test]
    fn recovery_generation_represents_one_replaceable_timer() {
        let mut coordinator = WorkspaceScanCoordinator::default();
        let first = coordinator.request_quiet_recovery();
        let second = coordinator.request_quiet_recovery();
        let latest = coordinator.request_quiet_recovery();

        assert_ne!(first, second);
        assert!(matches!(
            coordinator.recovery,
            WorkspaceRecoveryState::Debouncing {
                generation,
                minimum_scan_sequence: 1,
            } if generation == latest
        ));
        assert_eq!(coordinator.recovery_generation(), Some(latest));
        assert!(coordinator.request_recovery(first).is_none());
        assert_eq!(coordinator.debouncing_generation(), Some(latest));

        let scan = coordinator
            .request_recovery(latest)
            .expect("latest timer starts one scan");
        assert!(coordinator.recovery_generation().is_none());
        assert!(coordinator.request_recovery(second).is_none());
        assert!(!scan.cancellation.is_cancelled());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn replacing_and_dropping_the_recovery_timer_aborts_its_task() {
        let (first_tx, first_rx) = mpsc::channel();
        let (second_tx, second_rx) = mpsc::channel();
        let (first_started_tx, first_started_rx) = tokio::sync::oneshot::channel();
        let (second_started_tx, second_started_rx) = tokio::sync::oneshot::channel();
        let mut timer = WorkspaceScanRecoveryTimer::default();
        timer.replace(
            1,
            tokio::spawn(async move {
                let _on_drop = NotifyOnDrop(first_tx);
                let _ = first_started_tx.send(());
                std::future::pending::<()>().await;
            }),
        );
        assert_eq!(timer.generation(), Some(1));
        first_started_rx.await.expect("first timer started");
        timer.replace(
            2,
            tokio::spawn(async move {
                let _on_drop = NotifyOnDrop(second_tx);
                let _ = second_started_tx.send(());
                std::future::pending::<()>().await;
            }),
        );
        assert_eq!(timer.generation(), Some(2));
        second_started_rx.await.expect("second timer started");
        assert!(!timer.complete(1));
        assert_eq!(timer.generation(), Some(2));

        first_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("replacing the timer aborts the previous task");
        drop(timer);
        second_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("dropping the timer aborts the retained task");

        let mut completed = WorkspaceScanRecoveryTimer::default();
        completed.replace(3, tokio::spawn(async {}));
        assert!(completed.complete(3));
        assert_eq!(completed.generation(), None);
    }

    #[test]
    fn watched_change_journal_coalesces_and_bounds_uris() {
        let first = Url::parse("file:///workspace/first.adoc").expect("first URI");
        let second = Url::parse("file:///workspace/second.adoc").expect("second URI");
        let mut journal = WatchedChangeJournal::default();

        assert!(journal.record_with_limits(
            &[
                FileEvent::new(first.clone(), FileChangeType::CREATED),
                FileEvent::new(second.clone(), FileChangeType::CHANGED),
                FileEvent::new(first.clone(), FileChangeType::DELETED),
            ],
            2,
            first.as_str().len() + second.as_str().len(),
        ));
        let replay = journal.take().expect("replay");
        assert_eq!(
            replay.changes,
            vec![
                FileEvent::new(first.clone(), FileChangeType::DELETED),
                FileEvent::new(second.clone(), FileChangeType::CHANGED),
            ]
        );

        assert!(!journal.record_with_limits(
            &[
                FileEvent::new(first, FileChangeType::CHANGED),
                FileEvent::new(second, FileChangeType::CHANGED),
            ],
            1,
            usize::MAX,
        ));
        assert!(journal.take().is_none());
        assert!(journal.changes.is_empty());
    }

    #[test]
    fn journal_overflow_waits_for_quiet_before_restarting_the_worker() {
        let mut coordinator = WorkspaceScanCoordinator::default();
        let active = coordinator.request_replacement().expect("initial scan");
        let uri = Url::parse("file:///workspace/root.adoc").expect("URI");

        assert!(!coordinator.watched_changes.record_with_limits(
            &[FileEvent::new(uri.clone(), FileChangeType::CHANGED)],
            0,
            usize::MAX,
        ));
        let recovery = coordinator
            .record_watched_changes(&[FileEvent::new(uri, FileChangeType::CHANGED)])
            .expect("recovery generation");

        assert!(!active.cancellation.is_cancelled());
        assert!(!coordinator.accepts_active_result());
        assert!(coordinator.request_recovery(recovery).is_none());
        assert!(!active.cancellation.is_cancelled());
        let completion = coordinator
            .complete_active(active.sequence)
            .expect("completion");
        assert!(!completion.accept_result);
        assert!(completion.next.is_some());
        assert!(
            completion
                .rejection
                .as_deref()
                .is_some_and(|message| message.contains("watch journal limit exceeded"))
        );
    }

    #[test]
    fn accepted_scan_replays_watched_changes_after_installing_its_snapshot() {
        let (root, document_uri, mut service) = scan_race_service("adocweave-scan-replay");
        let mut coordinator = WorkspaceScanCoordinator::default();
        let active = coordinator.request_replacement().expect("active scan");
        let stale_scan = service.plan_workspace_scan(&adocweave::NeverCancel);
        fs::write(document_uri.to_file_path().expect("path"), "= After\n")
            .expect("changed document");
        let changes = vec![FileEvent::new(
            document_uri.clone(),
            FileChangeType::CHANGED,
        )];
        let _ = coordinator.record_watched_changes(&changes);
        let _ =
            service.workspace_files_changed_with_journal(DidChangeWatchedFilesParams { changes });

        let transition = coordinator
            .complete(
                &mut service,
                WorkspaceScanned {
                    sequence: active.sequence,
                    scan: Ok(stale_scan),
                },
            )
            .expect("scan completion");

        assert!(transition.next.is_none());
        assert_eq!(
            service
                .workspace_resource(&document_uri)
                .expect("updated resource")
                .as_ref(),
            "= After\n",
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn successful_scan_cancels_an_older_recovery_reservation() {
        let (root, _, mut service) = scan_race_service("adocweave-scan-clears-recovery");
        let mut coordinator = WorkspaceScanCoordinator::default();
        let active = coordinator.request_replacement().expect("active scan");
        let stale_recovery = coordinator.request_quiet_recovery();
        let scan = service.plan_workspace_scan(&adocweave::NeverCancel);

        let transition = coordinator
            .complete(
                &mut service,
                WorkspaceScanned {
                    sequence: active.sequence,
                    scan: Ok(scan),
                },
            )
            .expect("scan completion");

        assert!(matches!(
            transition.recovery_timer,
            WorkspaceRecoveryTimerUpdate::Cancel
        ));
        assert!(coordinator.recovery_generation().is_none());
        assert!(coordinator.request_recovery(stale_recovery).is_none());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn recovery_timer_before_completion_preserves_the_replay_journal() {
        let (root, document_uri, mut service) =
            scan_race_service("adocweave-recovery-before-completion");
        let mut coordinator = WorkspaceScanCoordinator::default();
        let active = coordinator.request_replacement().expect("active scan");
        let stale_scan = service.plan_workspace_scan(&adocweave::NeverCancel);
        fs::write(document_uri.to_file_path().expect("path"), "= Current\n")
            .expect("changed document");
        let changes = service.workspace_files_changed_with_journal(DidChangeWatchedFilesParams {
            changes: vec![FileEvent::new(
                document_uri.clone(),
                FileChangeType::CHANGED,
            )],
        });
        assert!(coordinator.record_workspace_changes(&changes).is_none());
        let recovery = coordinator.request_quiet_recovery();

        assert!(matches!(
            coordinator.recovery,
            WorkspaceRecoveryState::Debouncing {
                generation,
                minimum_scan_sequence,
            } if generation == recovery && minimum_scan_sequence == active.sequence
        ));
        assert!(coordinator.request_recovery(recovery).is_none());
        assert!(coordinator.accepts_active_result());
        assert_eq!(coordinator.watched_changes.changes.len(), 1);
        assert!(!coordinator.pending_replacement);
        assert!(matches!(
            coordinator.recovery,
            WorkspaceRecoveryState::AwaitingActiveCompletion {
                generation,
                minimum_scan_sequence,
            } if generation == recovery && minimum_scan_sequence == active.sequence
        ));
        assert!(coordinator.debouncing_generation().is_none());
        assert!(coordinator.request_recovery(recovery).is_none());

        let transition = coordinator
            .complete(
                &mut service,
                WorkspaceScanned {
                    sequence: active.sequence,
                    scan: Ok(stale_scan),
                },
            )
            .expect("scan completion");
        assert!(transition.next.is_none());
        assert!(matches!(
            transition.recovery_timer,
            WorkspaceRecoveryTimerUpdate::Cancel
        ));
        assert!(matches!(coordinator.recovery, WorkspaceRecoveryState::Idle));
        assert_eq!(
            service
                .workspace_resource(&document_uri)
                .expect("replayed resource")
                .as_ref(),
            "= Current\n"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn watched_change_rearms_recovery_while_active_completion_is_awaited() {
        let mut coordinator = WorkspaceScanCoordinator::default();
        let active = coordinator.request_replacement().expect("active scan");
        let first = coordinator.request_quiet_recovery();

        assert!(coordinator.request_recovery(first).is_none());
        assert!(matches!(
            coordinator.recovery,
            WorkspaceRecoveryState::AwaitingActiveCompletion {
                generation,
                minimum_scan_sequence,
            } if generation == first && minimum_scan_sequence == active.sequence
        ));

        let uri = Url::parse("file:///workspace/changed.adoc").expect("URI");
        let next = coordinator
            .record_watched_changes(&[FileEvent::new(uri, FileChangeType::CHANGED)])
            .expect("new timer generation");

        assert_ne!(next, first);
        assert!(matches!(
            coordinator.recovery,
            WorkspaceRecoveryState::Debouncing {
                generation,
                minimum_scan_sequence,
            } if generation == next && minimum_scan_sequence == active.sequence
        ));
        assert_eq!(coordinator.debouncing_generation(), Some(next));
        assert!(coordinator.request_recovery(first).is_none());
        assert_eq!(coordinator.debouncing_generation(), Some(next));
    }

    #[test]
    fn failed_successor_scan_preserves_incremental_state_after_recovery_timer() {
        let (root, document_uri, mut service) =
            scan_race_service("adocweave-failed-recovery-successor");
        let mut coordinator = WorkspaceScanCoordinator::default();
        let active = coordinator.request_replacement().expect("active scan");
        let stale_scan = service.plan_workspace_scan(&adocweave::NeverCancel);
        fs::write(document_uri.to_file_path().expect("path"), "= Current\n")
            .expect("changed document");
        let changes = service.workspace_files_changed_with_journal(DidChangeWatchedFilesParams {
            changes: vec![FileEvent::new(
                document_uri.clone(),
                FileChangeType::CHANGED,
            )],
        });
        let _ = coordinator.record_workspace_changes(&changes);
        let recovery = coordinator.request_unreplayable_recovery();
        assert!(coordinator.request_recovery(recovery).is_none());

        let first = coordinator
            .complete(
                &mut service,
                WorkspaceScanned {
                    sequence: active.sequence,
                    scan: Ok(stale_scan),
                },
            )
            .expect("old scan completion");
        let successor = first.next.expect("one recovery successor");
        assert_eq!(successor.sequence, active.sequence.saturating_add(1));
        assert_eq!(
            service
                .workspace_resource(&document_uri)
                .expect("incremental resource")
                .as_ref(),
            "= Current\n"
        );

        let failed = coordinator
            .complete(
                &mut service,
                WorkspaceScanned {
                    sequence: successor.sequence,
                    scan: Err("recovery worker failed".to_owned()),
                },
            )
            .expect("failed recovery completion");
        assert!(failed.next.is_none());
        assert_eq!(
            service
                .workspace_resource(&document_uri)
                .expect("retained incremental resource")
                .as_ref(),
            "= Current\n"
        );

        let retry_change =
            service.workspace_files_changed_with_journal(DidChangeWatchedFilesParams {
                changes: vec![FileEvent::new(
                    document_uri.clone(),
                    FileChangeType::CHANGED,
                )],
            });
        assert!(retry_change.recovery_required);
        let retry_timer = coordinator
            .record_workspace_changes(&retry_change)
            .expect("retry recovery reservation");
        let retry = coordinator
            .request_recovery(retry_timer)
            .expect("retry scan after quiet period");
        assert!(retry.sequence > successor.sequence);
        let retry_scan = service.plan_workspace_scan(&adocweave::NeverCancel);
        let recovered = coordinator
            .complete(
                &mut service,
                WorkspaceScanned {
                    sequence: retry.sequence,
                    scan: Ok(retry_scan),
                },
            )
            .expect("successful retry completion");
        assert!(matches!(
            recovered.recovery_timer,
            WorkspaceRecoveryTimerUpdate::Cancel
        ));
        assert!(coordinator.recovery_generation().is_none());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn configuration_replacement_supersedes_recovery_and_converges_once() {
        let (root, document_uri, mut service) =
            scan_race_service("adocweave-config-recovery-order");
        let mut coordinator = WorkspaceScanCoordinator::default();
        let active = coordinator.request_replacement().expect("active scan");
        let uri_change = FileEvent::new(document_uri, FileChangeType::CHANGED);
        let _ = coordinator.record_watched_changes(&[uri_change]);
        let stale_recovery = coordinator.request_quiet_recovery();

        assert!(coordinator.request_replacement().is_none());
        assert!(active.cancellation.is_cancelled());
        assert!(coordinator.request_recovery(stale_recovery).is_none());
        let replaced = coordinator
            .complete_active(active.sequence)
            .expect("cancelled completion");
        let replacement = replaced.next.expect("one structural replacement");
        assert!(!replacement.cancellation.is_cancelled());
        assert!(coordinator.watched_changes.changes.is_empty());

        let scan = service.plan_workspace_scan(&adocweave::NeverCancel);
        let completed = coordinator
            .complete(
                &mut service,
                WorkspaceScanned {
                    sequence: replacement.sequence,
                    scan: Ok(scan),
                },
            )
            .expect("replacement completion");
        assert!(completed.next.is_none());
        assert!(coordinator.recovery_generation().is_none());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn watched_change_after_completion_updates_the_installed_snapshot() {
        let (root, document_uri, mut service) = scan_race_service("adocweave-watch-after-scan");
        let mut coordinator = WorkspaceScanCoordinator::default();
        let active = coordinator.request_replacement().expect("active scan");
        let scan = service.plan_workspace_scan(&adocweave::NeverCancel);
        let transition = coordinator
            .complete(
                &mut service,
                WorkspaceScanned {
                    sequence: active.sequence,
                    scan: Ok(scan),
                },
            )
            .expect("scan completion");
        assert!(transition.next.is_none());

        fs::write(document_uri.to_file_path().expect("path"), "= After\n")
            .expect("changed document");
        let outcome = service.workspace_files_changed_with_journal(DidChangeWatchedFilesParams {
            changes: vec![FileEvent::new(
                document_uri.clone(),
                FileChangeType::CHANGED,
            )],
        });

        assert!(!outcome.recovery_required);
        assert_eq!(
            service
                .workspace_resource(&document_uri)
                .expect("updated resource")
                .as_ref(),
            "= After\n",
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn replay_propagates_a_new_recovery_requirement() {
        let (root, _, mut service) = scan_race_service("adocweave-scan-replay-recovery");
        let mut coordinator = WorkspaceScanCoordinator::default();
        let active = coordinator.request_replacement().expect("active scan");
        let scan = service.plan_workspace_scan(&adocweave::NeverCancel);
        let changes = (0..129)
            .map(|index| {
                let path = root.join(format!("missing-{index}.adoc"));
                FileEvent::new(
                    Url::from_file_path(path).expect("file URI"),
                    FileChangeType::CREATED,
                )
            })
            .collect::<Vec<_>>();
        assert!(coordinator.record_watched_changes(&changes).is_none());
        let first_pass =
            service.workspace_files_changed_with_journal(DidChangeWatchedFilesParams {
                changes: changes.clone(),
            });
        assert!(first_pass.recovery_required);

        let transition = coordinator
            .complete(
                &mut service,
                WorkspaceScanned {
                    sequence: active.sequence,
                    scan: Ok(scan),
                },
            )
            .expect("scan completion");

        let WorkspaceRecoveryTimerUpdate::Arm(recovery) = transition.recovery_timer else {
            panic!("replay must arm recovery");
        };
        assert_eq!(coordinator.recovery_generation(), Some(recovery));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn unreplayable_watch_batch_survives_completion_of_the_older_scan() {
        let (root, document_uri, mut service) = scan_race_service("adocweave-unreplayable-watch");
        let mut coordinator = WorkspaceScanCoordinator::default();
        let active = coordinator.request_replacement().expect("active scan");
        let stale_scan = service.plan_workspace_scan(&adocweave::NeverCancel);

        fs::write(document_uri.to_file_path().expect("path"), "= Live\n")
            .expect("changed document");
        let replayable =
            service.workspace_files_changed_with_journal(DidChangeWatchedFilesParams {
                changes: vec![FileEvent::new(
                    document_uri.clone(),
                    FileChangeType::CHANGED,
                )],
            });
        assert!(replayable.replay_complete);
        assert!(coordinator.record_workspace_changes(&replayable).is_none());

        let oversized = service.workspace_files_changed_with_journal(DidChangeWatchedFilesParams {
            changes: (0..=10_000)
                .map(|index| {
                    FileEvent::new(
                        Url::from_file_path(root.join(format!("f{index}.adoc"))).expect("file URI"),
                        FileChangeType::CREATED,
                    )
                })
                .collect(),
        });
        assert!(oversized.recovery_required);
        assert!(!oversized.replay_complete);
        let recovery = coordinator
            .record_workspace_changes(&oversized)
            .expect("recovery reservation");
        assert!(!coordinator.accepts_active_result());
        assert_eq!(
            coordinator.recovery_minimum_scan_sequence(),
            Some(active.sequence.saturating_add(1))
        );

        let transition = coordinator
            .complete(
                &mut service,
                WorkspaceScanned {
                    sequence: active.sequence,
                    scan: Ok(stale_scan),
                },
            )
            .expect("old scan completion");

        assert!(matches!(
            transition.recovery_timer,
            WorkspaceRecoveryTimerUpdate::Keep
        ));
        assert_eq!(
            coordinator.recovery_generation(),
            Some(recovery),
            "the older scan cannot discharge recovery that requires its successor"
        );
        assert_eq!(
            service
                .workspace_resource(&document_uri)
                .expect("retained live resource")
                .as_ref(),
            "= Live\n",
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn journal_overflow_keeps_incremental_state_and_finishes_with_a_bounded_error() {
        let (root, document_uri, mut service) = scan_race_service("adocweave-scan-overflow");
        let mut coordinator = WorkspaceScanCoordinator::default();
        let active = coordinator.request_replacement().expect("active scan");
        let stale_scan = service.plan_workspace_scan(&adocweave::NeverCancel);
        fs::write(document_uri.to_file_path().expect("path"), "= After\n")
            .expect("changed document");
        let changes = vec![FileEvent::new(
            document_uri.clone(),
            FileChangeType::CHANGED,
        )];
        assert!(
            !coordinator
                .watched_changes
                .record_with_limits(&changes, 0, usize::MAX)
        );
        let first_recovery = coordinator
            .record_watched_changes(&changes)
            .expect("first recovery generation");
        let _ =
            service.workspace_files_changed_with_journal(DidChangeWatchedFilesParams { changes });

        let transition = coordinator
            .complete(
                &mut service,
                WorkspaceScanned {
                    sequence: active.sequence,
                    scan: Ok(stale_scan),
                },
            )
            .expect("scan completion");

        assert!(
            transition.next.is_none(),
            "recovery waits for the quiet timer"
        );
        assert_eq!(
            service
                .workspace_resource(&document_uri)
                .expect("incremental resource")
                .as_ref(),
            "= After\n",
            "the rejected snapshot must not replace the watched update",
        );
        let diagnostics = service.diagnostics(&document_uri).expect("diagnostics");
        assert!(diagnostics.diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("workspace watch journal limit exceeded")
        }));
        let final_recovery = coordinator
            .record_watched_changes(&[FileEvent::new(document_uri, FileChangeType::CHANGED)])
            .expect("updated recovery generation");
        assert!(coordinator.request_recovery(first_recovery).is_none());
        let recovery = coordinator
            .request_recovery(final_recovery)
            .expect("one recovery after notifications stop");
        assert!(
            !recovery.cancellation.is_cancelled(),
            "the bounded recovery worker starts after the quiet period"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn accepted_worker_failure_is_reported_without_replacing_the_workspace() {
        let (root, document_uri, mut service) = scan_race_service("adocweave-scan-join-error");
        let previous = service
            .workspace_resource(&document_uri)
            .expect("workspace resource")
            .clone();
        let mut coordinator = WorkspaceScanCoordinator::default();
        let active = coordinator.request_replacement().expect("active scan");

        let transition = coordinator
            .complete(
                &mut service,
                WorkspaceScanned {
                    sequence: active.sequence,
                    scan: Err("workspace scan worker failed: panic".to_owned()),
                },
            )
            .expect("scan completion");

        assert!(transition.next.is_none());
        assert_eq!(
            service
                .workspace_resource(&document_uri)
                .expect("retained resource"),
            previous,
        );
        let diagnostics = service.diagnostics(&document_uri).expect("diagnostics");
        assert!(
            diagnostics
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("workspace scan worker failed"))
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn rejected_worker_failure_starts_the_replacement_without_a_diagnostic() {
        let (root, document_uri, mut service) =
            scan_race_service("adocweave-rejected-scan-join-error");
        let mut coordinator = WorkspaceScanCoordinator::default();
        let old = coordinator.request_replacement().expect("active scan");
        assert!(coordinator.request_replacement().is_none());

        let transition = coordinator
            .complete(
                &mut service,
                WorkspaceScanned {
                    sequence: old.sequence,
                    scan: Err("workspace scan worker failed: cancelled panic".to_owned()),
                },
            )
            .expect("scan completion");

        assert!(transition.jobs.is_empty());
        assert!(transition.next.is_some());
        let diagnostics = service.diagnostics(&document_uri).expect("diagnostics");
        assert!(
            diagnostics
                .diagnostics
                .iter()
                .all(|diagnostic| !diagnostic.message.contains("workspace scan worker failed"))
        );
        fs::remove_dir_all(root).expect("cleanup");
    }
}
