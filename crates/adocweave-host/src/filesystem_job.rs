//! Job-wide limits for filesystem work.
//!
//! The public coordinator exposes identity, limits, usage and lifecycle. I/O
//! permits and reservations stay inside this crate so callers cannot separate
//! session registration, operation charging and bounded reads.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use crate::LocalFilesystemSessionId;

static NEXT_JOB_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FilesystemJobId(u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FilesystemJobLimits {
    /// Maximum file-read attempts, including failures and cache hits.
    pub max_read_operations: u64,
    /// Maximum bytes returned by file reads before overflow probing.
    pub max_read_bytes: u64,
    /// Bytes available to distinguish exact EOF from a read-byte overflow.
    ///
    /// A value of zero fails closed when the normal limit is reached because
    /// the coordinator cannot inspect EOF.
    pub max_read_probe_bytes: u64,
    /// Maximum directory-open attempts, including failed opens.
    pub max_directory_operations: u64,
    /// Maximum logical directory entries before overflow probing.
    pub max_directory_entries: u64,
    /// Entries available to distinguish exact EOF from an entry overflow.
    ///
    /// A value of zero fails closed when the normal limit is reached because
    /// the coordinator cannot advance the iterator to inspect EOF.
    pub max_directory_probe_entries: u64,
    /// Maximum accepted changes to draft candidate state.
    pub max_candidate_changes: u64,
    /// Maximum distinct filesystem sessions participating in the job.
    pub max_sessions: usize,
}

impl FilesystemJobLimits {
    /// Creates limits which do not impose a practical bound.
    ///
    /// Consumers should normally select finite limits. This value is useful
    /// while migrating code which already applies a stricter independent
    /// filesystem policy.
    pub const fn unbounded() -> Self {
        Self {
            max_read_operations: u64::MAX,
            max_read_bytes: u64::MAX,
            max_read_probe_bytes: 1,
            max_directory_operations: u64::MAX,
            max_directory_entries: u64::MAX,
            max_directory_probe_entries: 1,
            max_candidate_changes: u64::MAX,
            max_sessions: usize::MAX,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FilesystemJobUsage {
    pub read_operations: u64,
    pub read_bytes: u64,
    pub reserved_read_bytes: u64,
    pub read_probe_bytes: u64,
    pub reserved_read_probe_bytes: u64,
    pub directory_operations: u64,
    pub directory_entries: u64,
    pub reserved_directory_entries: u64,
    pub directory_probe_entries: u64,
    pub reserved_directory_probe_entries: u64,
    pub candidate_changes: u64,
    pub sessions: usize,
    pub active_operations: u64,
    pub waiting_reservations: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilesystemJobLimit {
    ReadOperations { limit: u64 },
    ReadBytes { limit: u64 },
    ReadProbeBytes { limit: u64 },
    DirectoryOperations { limit: u64 },
    DirectoryEntries { limit: u64 },
    DirectoryProbeEntries { limit: u64 },
    CandidateChanges { limit: u64 },
    Sessions { limit: usize },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FilesystemJobError {
    IdExhausted,
    Finished,
    Cancelled,
    Limit(FilesystemJobLimit),
    InFlight { operations: u64, reservations: u64 },
    AccountingViolation { granted: u64, observed: u64 },
    StatePoisoned,
}

impl fmt::Display for FilesystemJobError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IdExhausted => formatter.write_str("filesystem job identity is exhausted"),
            Self::Finished => formatter.write_str("filesystem job has finished"),
            Self::Cancelled => formatter.write_str("filesystem job was cancelled"),
            Self::Limit(limit) => write!(formatter, "filesystem job limit exceeded: {limit}"),
            Self::InFlight {
                operations,
                reservations,
            } => write!(
                formatter,
                "filesystem job still has {operations} operations and {reservations} reserved units in flight",
            ),
            Self::AccountingViolation { granted, observed } => write!(
                formatter,
                "filesystem job observed {observed} units from a reservation of {granted}",
            ),
            Self::StatePoisoned => formatter.write_str("filesystem job state lock is poisoned"),
        }
    }
}

impl Error for FilesystemJobError {}

impl fmt::Display for FilesystemJobLimit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadOperations { limit } => write!(formatter, "read operations ({limit})"),
            Self::ReadBytes { limit } => write!(formatter, "read bytes ({limit})"),
            Self::ReadProbeBytes { limit } => write!(formatter, "read probe bytes ({limit})"),
            Self::DirectoryOperations { limit } => {
                write!(formatter, "directory operations ({limit})")
            }
            Self::DirectoryEntries { limit } => write!(formatter, "directory entries ({limit})"),
            Self::DirectoryProbeEntries { limit } => {
                write!(formatter, "directory probe entries ({limit})")
            }
            Self::CandidateChanges { limit } => write!(formatter, "candidate changes ({limit})"),
            Self::Sessions { limit } => write!(formatter, "participating sessions ({limit})"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct FilesystemJobCoordinator {
    inner: Arc<JobInner>,
}

#[derive(Debug)]
struct JobInner {
    id: FilesystemJobId,
    limits: FilesystemJobLimits,
    state: Mutex<JobState>,
    changed: Condvar,
}

#[derive(Debug, Default)]
struct JobState {
    terminal: Option<JobTerminal>,
    usage: FilesystemJobUsage,
    sessions: BTreeSet<LocalFilesystemSessionId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JobTerminal {
    Finished,
    Cancelled,
    Limit(FilesystemJobLimit),
    AccountingViolation { granted: u64, observed: u64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OperationKind {
    Read,
    Directory,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CapacityKind {
    Read,
    DirectoryEntry,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CapacityClass {
    Normal,
    Probe,
}

#[derive(Debug)]
pub(crate) struct FilesystemReadPermit {
    coordinator: FilesystemJobCoordinator,
    active: bool,
}

#[derive(Debug)]
pub(crate) struct FilesystemDirectoryPermit {
    coordinator: FilesystemJobCoordinator,
    active: bool,
}

#[derive(Debug)]
pub(crate) struct FilesystemCapacityReservation<'a> {
    coordinator: &'a FilesystemJobCoordinator,
    kind: CapacityKind,
    class: CapacityClass,
    granted: u64,
    active: bool,
}

impl FilesystemJobCoordinator {
    pub fn new(limits: FilesystemJobLimits) -> Result<Self, FilesystemJobError> {
        let id = NEXT_JOB_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .map(FilesystemJobId)
            .map_err(|_| FilesystemJobError::IdExhausted)?;
        Ok(Self {
            inner: Arc::new(JobInner {
                id,
                limits,
                state: Mutex::new(JobState::default()),
                changed: Condvar::new(),
            }),
        })
    }

    pub fn id(&self) -> FilesystemJobId {
        self.inner.id
    }

    pub fn limits(&self) -> FilesystemJobLimits {
        self.inner.limits
    }

    pub fn usage(&self) -> Result<FilesystemJobUsage, FilesystemJobError> {
        Ok(self.lock()?.usage)
    }

    pub(crate) fn ensure_active_job(&self) -> Result<(), FilesystemJobError> {
        let state = self.lock()?;
        Self::ensure_active(&state)
    }

    pub(crate) fn with_active_commit<T>(
        &self,
        commit: impl FnOnce() -> T,
    ) -> Result<T, FilesystemJobError> {
        let state = self.lock_active()?;
        let result = commit();
        drop(state);
        Ok(result)
    }

    pub fn finish(&self) -> Result<(), FilesystemJobError> {
        let mut state = self.lock()?;
        match state.terminal {
            Some(JobTerminal::Finished) => return Ok(()),
            Some(terminal) => return Err(error_from_terminal(terminal)),
            None => {}
        }
        let reservations = reservation_total(&state.usage);
        if state.usage.active_operations > 0 || reservations > 0 {
            return Err(FilesystemJobError::InFlight {
                operations: state.usage.active_operations,
                reservations,
            });
        }
        state.terminal = Some(JobTerminal::Finished);
        self.inner.changed.notify_all();
        Ok(())
    }

    pub fn cancel(&self) -> Result<(), FilesystemJobError> {
        let mut state = self.lock()?;
        match state.terminal {
            Some(JobTerminal::Cancelled) => return Ok(()),
            Some(terminal) => return Err(error_from_terminal(terminal)),
            None => {}
        }
        state.terminal = Some(JobTerminal::Cancelled);
        self.inner.changed.notify_all();
        Ok(())
    }

    pub(crate) fn begin_read(
        &self,
        session: LocalFilesystemSessionId,
    ) -> Result<FilesystemReadPermit, FilesystemJobError> {
        self.begin_operation(session, OperationKind::Read)?;
        Ok(FilesystemReadPermit {
            coordinator: self.clone(),
            active: true,
        })
    }

    pub(crate) fn register_session(
        &self,
        session: LocalFilesystemSessionId,
    ) -> Result<(), FilesystemJobError> {
        let mut state = self.lock_active()?;
        if state.sessions.contains(&session) {
            return Ok(());
        }
        if state.sessions.len() >= self.inner.limits.max_sessions {
            return Err(self.stop_at_limit(
                &mut state,
                FilesystemJobLimit::Sessions {
                    limit: self.inner.limits.max_sessions,
                },
            ));
        }
        state.sessions.insert(session);
        state.usage.sessions = state.sessions.len();
        Ok(())
    }

    pub(crate) fn begin_directory_read(
        &self,
        session: LocalFilesystemSessionId,
    ) -> Result<FilesystemDirectoryPermit, FilesystemJobError> {
        self.begin_operation(session, OperationKind::Directory)?;
        Ok(FilesystemDirectoryPermit {
            coordinator: self.clone(),
            active: true,
        })
    }

    pub(crate) fn record_candidate_change(&self) -> Result<(), FilesystemJobError> {
        let mut state = self.lock_active()?;
        let limit = self.inner.limits.max_candidate_changes;
        if state.usage.candidate_changes >= limit {
            return Err(
                self.stop_at_limit(&mut state, FilesystemJobLimit::CandidateChanges { limit })
            );
        }
        state.usage.candidate_changes += 1;
        Ok(())
    }

    fn begin_operation(
        &self,
        session: LocalFilesystemSessionId,
        kind: OperationKind,
    ) -> Result<(), FilesystemJobError> {
        let mut state = self.lock_active()?;
        let new_session = !state.sessions.contains(&session);
        if new_session && state.sessions.len() >= self.inner.limits.max_sessions {
            return Err(self.stop_at_limit(
                &mut state,
                FilesystemJobLimit::Sessions {
                    limit: self.inner.limits.max_sessions,
                },
            ));
        }
        let (current, limit, error) = match kind {
            OperationKind::Read => (
                state.usage.read_operations,
                self.inner.limits.max_read_operations,
                FilesystemJobLimit::ReadOperations {
                    limit: self.inner.limits.max_read_operations,
                },
            ),
            OperationKind::Directory => (
                state.usage.directory_operations,
                self.inner.limits.max_directory_operations,
                FilesystemJobLimit::DirectoryOperations {
                    limit: self.inner.limits.max_directory_operations,
                },
            ),
        };
        if current >= limit {
            return Err(self.stop_at_limit(&mut state, error));
        }
        if new_session {
            state.sessions.insert(session);
            state.usage.sessions = state.sessions.len();
        }
        match kind {
            OperationKind::Read => state.usage.read_operations += 1,
            OperationKind::Directory => state.usage.directory_operations += 1,
        }
        state.usage.active_operations += 1;
        Ok(())
    }

    fn reserve(
        &self,
        kind: CapacityKind,
        requested: u64,
    ) -> Result<(CapacityClass, u64), FilesystemJobError> {
        let mut state = self.lock_active()?;
        loop {
            Self::ensure_active(&state)?;
            let values = capacity_values(&state.usage, self.inner.limits, kind);
            let normal_available = values
                .normal_limit
                .saturating_sub(values.normal_committed)
                .saturating_sub(values.normal_reserved);
            if requested == 0 || normal_available > 0 {
                let granted = requested.min(normal_available);
                *reserved_mut(&mut state.usage, kind, CapacityClass::Normal) += granted;
                return Ok((CapacityClass::Normal, granted));
            }
            if values.normal_committed < values.normal_limit {
                state.usage.waiting_reservations += 1;
                state = self
                    .inner
                    .changed
                    .wait(state)
                    .map_err(|_| FilesystemJobError::StatePoisoned)?;
                state.usage.waiting_reservations -= 1;
                continue;
            }
            let probe_available = values
                .probe_limit
                .saturating_sub(values.probe_committed)
                .saturating_sub(values.probe_reserved);
            if probe_available > 0 {
                let granted = requested.min(probe_available);
                *reserved_mut(&mut state.usage, kind, CapacityClass::Probe) += granted;
                return Ok((CapacityClass::Probe, granted));
            }
            if values.probe_committed < values.probe_limit {
                state.usage.waiting_reservations += 1;
                state = self
                    .inner
                    .changed
                    .wait(state)
                    .map_err(|_| FilesystemJobError::StatePoisoned)?;
                state.usage.waiting_reservations -= 1;
                continue;
            }
            return Err(self.stop_at_limit(&mut state, values.probe_limit_error));
        }
    }

    fn release_operation(&self) {
        if let Ok(mut state) = self.lock() {
            state.usage.active_operations = state
                .usage
                .active_operations
                .checked_sub(1)
                .expect("active permit is counted by its coordinator");
            self.inner.changed.notify_all();
        }
    }

    fn lock(&self) -> Result<MutexGuard<'_, JobState>, FilesystemJobError> {
        self.inner
            .state
            .lock()
            .map_err(|_| FilesystemJobError::StatePoisoned)
    }

    fn lock_active(&self) -> Result<MutexGuard<'_, JobState>, FilesystemJobError> {
        let state = self.lock()?;
        Self::ensure_active(&state)?;
        Ok(state)
    }

    fn ensure_active(state: &JobState) -> Result<(), FilesystemJobError> {
        match state.terminal {
            None => Ok(()),
            Some(terminal) => Err(error_from_terminal(terminal)),
        }
    }

    fn stop_at_limit(&self, state: &mut JobState, limit: FilesystemJobLimit) -> FilesystemJobError {
        state.terminal = Some(JobTerminal::Limit(limit));
        self.inner.changed.notify_all();
        FilesystemJobError::Limit(limit)
    }
}

impl FilesystemReadPermit {
    pub(crate) fn reserve(
        &mut self,
        requested: u64,
    ) -> Result<FilesystemCapacityReservation<'_>, FilesystemJobError> {
        let (class, granted) = self.coordinator.reserve(CapacityKind::Read, requested)?;
        Ok(FilesystemCapacityReservation {
            coordinator: &self.coordinator,
            kind: CapacityKind::Read,
            class,
            granted,
            active: true,
        })
    }
}

impl FilesystemDirectoryPermit {
    pub(crate) fn reserve_entry(
        &mut self,
    ) -> Result<FilesystemCapacityReservation<'_>, FilesystemJobError> {
        let (class, granted) = self.coordinator.reserve(CapacityKind::DirectoryEntry, 1)?;
        Ok(FilesystemCapacityReservation {
            coordinator: &self.coordinator,
            kind: CapacityKind::DirectoryEntry,
            class,
            granted,
            active: true,
        })
    }
}

impl Drop for FilesystemReadPermit {
    fn drop(&mut self) {
        if self.active {
            self.coordinator.release_operation();
            self.active = false;
        }
    }
}

impl Drop for FilesystemDirectoryPermit {
    fn drop(&mut self) {
        if self.active {
            self.coordinator.release_operation();
            self.active = false;
        }
    }
}

impl FilesystemCapacityReservation<'_> {
    pub(crate) const fn granted(&self) -> u64 {
        self.granted
    }

    #[cfg(test)]
    pub(crate) const fn is_probe(&self) -> bool {
        matches!(self.class, CapacityClass::Probe)
    }

    pub(crate) fn commit(mut self, observed: u64) -> Result<(), FilesystemJobError> {
        let mut state = self.coordinator.lock()?;
        let existing_terminal = state.terminal;
        *reserved_mut(&mut state.usage, self.kind, self.class) =
            reserved_mut(&mut state.usage, self.kind, self.class)
                .checked_sub(self.granted)
                .expect("active reservation is counted by its coordinator");
        let recorded = observed.min(self.granted);
        *committed_mut(&mut state.usage, self.kind, self.class) += recorded;
        self.active = false;
        if observed > self.granted {
            let violation = JobTerminal::AccountingViolation {
                granted: self.granted,
                observed,
            };
            state.terminal.get_or_insert(violation);
            self.coordinator.inner.changed.notify_all();
            return Err(error_from_terminal(existing_terminal.unwrap_or(violation)));
        }
        if self.class == CapacityClass::Probe && observed > 0 {
            let limit = capacity_values(&state.usage, self.coordinator.inner.limits, self.kind)
                .normal_limit_error;
            state.terminal.get_or_insert(JobTerminal::Limit(limit));
            self.coordinator.inner.changed.notify_all();
            return Err(error_from_terminal(
                existing_terminal.unwrap_or(JobTerminal::Limit(limit)),
            ));
        }
        if let Some(terminal) = existing_terminal {
            self.coordinator.inner.changed.notify_all();
            return Err(error_from_terminal(terminal));
        }
        self.coordinator.inner.changed.notify_all();
        Ok(())
    }
}

fn error_from_terminal(terminal: JobTerminal) -> FilesystemJobError {
    match terminal {
        JobTerminal::Finished => FilesystemJobError::Finished,
        JobTerminal::Cancelled => FilesystemJobError::Cancelled,
        JobTerminal::Limit(limit) => FilesystemJobError::Limit(limit),
        JobTerminal::AccountingViolation { granted, observed } => {
            FilesystemJobError::AccountingViolation { granted, observed }
        }
    }
}

impl Drop for FilesystemCapacityReservation<'_> {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if let Ok(mut state) = self.coordinator.lock() {
            *reserved_mut(&mut state.usage, self.kind, self.class) =
                reserved_mut(&mut state.usage, self.kind, self.class)
                    .checked_sub(self.granted)
                    .expect("active reservation is counted by its coordinator");
            self.coordinator.inner.changed.notify_all();
        }
    }
}

#[derive(Clone, Copy)]
struct CapacityValues {
    normal_committed: u64,
    normal_reserved: u64,
    normal_limit: u64,
    probe_committed: u64,
    probe_reserved: u64,
    probe_limit: u64,
    normal_limit_error: FilesystemJobLimit,
    probe_limit_error: FilesystemJobLimit,
}

fn capacity_values(
    usage: &FilesystemJobUsage,
    limits: FilesystemJobLimits,
    kind: CapacityKind,
) -> CapacityValues {
    match kind {
        CapacityKind::Read => CapacityValues {
            normal_committed: usage.read_bytes,
            normal_reserved: usage.reserved_read_bytes,
            normal_limit: limits.max_read_bytes,
            probe_committed: usage.read_probe_bytes,
            probe_reserved: usage.reserved_read_probe_bytes,
            probe_limit: limits.max_read_probe_bytes,
            normal_limit_error: FilesystemJobLimit::ReadBytes {
                limit: limits.max_read_bytes,
            },
            probe_limit_error: FilesystemJobLimit::ReadProbeBytes {
                limit: limits.max_read_probe_bytes,
            },
        },
        CapacityKind::DirectoryEntry => CapacityValues {
            normal_committed: usage.directory_entries,
            normal_reserved: usage.reserved_directory_entries,
            normal_limit: limits.max_directory_entries,
            probe_committed: usage.directory_probe_entries,
            probe_reserved: usage.reserved_directory_probe_entries,
            probe_limit: limits.max_directory_probe_entries,
            normal_limit_error: FilesystemJobLimit::DirectoryEntries {
                limit: limits.max_directory_entries,
            },
            probe_limit_error: FilesystemJobLimit::DirectoryProbeEntries {
                limit: limits.max_directory_probe_entries,
            },
        },
    }
}

fn reserved_mut(
    usage: &mut FilesystemJobUsage,
    kind: CapacityKind,
    class: CapacityClass,
) -> &mut u64 {
    match (kind, class) {
        (CapacityKind::Read, CapacityClass::Normal) => &mut usage.reserved_read_bytes,
        (CapacityKind::Read, CapacityClass::Probe) => &mut usage.reserved_read_probe_bytes,
        (CapacityKind::DirectoryEntry, CapacityClass::Normal) => {
            &mut usage.reserved_directory_entries
        }
        (CapacityKind::DirectoryEntry, CapacityClass::Probe) => {
            &mut usage.reserved_directory_probe_entries
        }
    }
}

fn committed_mut(
    usage: &mut FilesystemJobUsage,
    kind: CapacityKind,
    class: CapacityClass,
) -> &mut u64 {
    match (kind, class) {
        (CapacityKind::Read, CapacityClass::Normal) => &mut usage.read_bytes,
        (CapacityKind::Read, CapacityClass::Probe) => &mut usage.read_probe_bytes,
        (CapacityKind::DirectoryEntry, CapacityClass::Normal) => &mut usage.directory_entries,
        (CapacityKind::DirectoryEntry, CapacityClass::Probe) => &mut usage.directory_probe_entries,
    }
}

fn reservation_total(usage: &FilesystemJobUsage) -> u64 {
    usage
        .reserved_read_bytes
        .saturating_add(usage.reserved_read_probe_bytes)
        .saturating_add(usage.reserved_directory_entries)
        .saturating_add(usage.reserved_directory_probe_entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FilesystemReadLimits, LocalFilesystemPolicy};
    use std::fs;
    use std::sync::{Arc, Barrier};
    use std::thread;

    static NEXT_TEST_ROOT: AtomicU64 = AtomicU64::new(1);

    fn limits() -> FilesystemJobLimits {
        FilesystemJobLimits {
            max_read_operations: 3,
            max_read_bytes: 4,
            max_read_probe_bytes: 1,
            max_directory_operations: 2,
            max_directory_entries: 2,
            max_directory_probe_entries: 1,
            max_candidate_changes: 2,
            max_sessions: 2,
        }
    }

    fn sessions(count: usize) -> (std::path::PathBuf, Vec<crate::LocalFilesystemSession>) {
        let root = std::env::temp_dir().join(format!(
            "adocweave-filesystem-job-sessions-{}-{}",
            std::process::id(),
            NEXT_TEST_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).expect("root");
        let policy = LocalFilesystemPolicy::new([root.clone()], FilesystemReadLimits::default())
            .expect("policy");
        let sessions = (0..count)
            .map(|_| policy.session().expect("session"))
            .collect();
        (root, sessions)
    }

    #[test]
    fn permit_atomically_registers_a_session_and_read_operation() {
        let (root, sessions) = sessions(3);
        let job = FilesystemJobCoordinator::new(limits()).expect("job");
        let first = job.begin_read(sessions[0].session_id()).expect("first");
        let repeated = job.begin_read(sessions[0].session_id()).expect("repeated");
        drop((first, repeated));
        let second = job.begin_read(sessions[1].session_id()).expect("second");
        drop(second);
        assert_eq!(job.usage().expect("usage").sessions, 2);
        assert_eq!(job.usage().expect("usage").read_operations, 3);
        assert_eq!(
            job.begin_read(sessions[2].session_id())
                .expect_err("session limit"),
            FilesystemJobError::Limit(FilesystemJobLimit::Sessions { limit: 2 })
        );
        drop(sessions);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn partial_commit_returns_only_unused_capacity() {
        let (root, sessions) = sessions(1);
        let job = FilesystemJobCoordinator::new(limits()).expect("job");
        let mut permit = job.begin_read(sessions[0].session_id()).expect("permit");
        let reservation = permit.reserve(4).expect("reservation");
        reservation.commit(2).expect("partial commit");
        let discarded = permit.reserve(2).expect("discarded");
        drop(discarded);
        assert_eq!(job.usage().expect("usage").read_bytes, 2);
        drop(permit);
        drop(sessions);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn concurrent_permits_never_reserve_the_same_bytes() {
        let (root, sessions) = sessions(2);
        let job = FilesystemJobCoordinator::new(limits()).expect("job");
        let mut first_permit = job.begin_read(sessions[0].session_id()).expect("first");
        let first = first_permit.reserve(4).expect("first reservation");
        let waiting_job = job.clone();
        let second_id = sessions[1].session_id();
        let barrier = Arc::new(Barrier::new(2));
        let waiting_barrier = barrier.clone();
        let waiter = thread::spawn(move || {
            let mut permit = waiting_job.begin_read(second_id).expect("second permit");
            waiting_barrier.wait();
            let reservation = permit.reserve(4).expect("waited reservation");
            let granted = reservation.granted();
            reservation.commit(granted).expect("waited commit");
            granted
        });
        barrier.wait();
        while job.usage().expect("usage").waiting_reservations == 0 {
            thread::yield_now();
        }
        first.commit(1).expect("first commit");
        assert_eq!(waiter.join().expect("waiter"), 3);
        assert_eq!(job.usage().expect("usage").read_bytes, 4);
        drop(first_permit);
        drop(sessions);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn probe_eof_is_free_but_observed_data_stops_the_job_at_the_main_limit() {
        let (root, sessions) = sessions(1);
        let job = FilesystemJobCoordinator::new(limits()).expect("job");
        let mut permit = job.begin_read(sessions[0].session_id()).expect("permit");
        permit
            .reserve(4)
            .expect("normal")
            .commit(4)
            .expect("normal commit");
        let eof = permit.reserve(1).expect("EOF probe");
        assert!(eof.is_probe());
        eof.commit(0).expect("EOF");
        let excess = permit.reserve(1).expect("excess probe");
        assert_eq!(
            excess.commit(1),
            Err(FilesystemJobError::Limit(FilesystemJobLimit::ReadBytes {
                limit: 4
            }))
        );
        assert_eq!(job.usage().expect("usage").read_probe_bytes, 1);
        drop(permit);
        drop(sessions);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn accounting_violation_is_terminal_and_keeps_the_granted_amount() {
        let (root, sessions) = sessions(1);
        let job = FilesystemJobCoordinator::new(limits()).expect("job");
        let mut permit = job.begin_read(sessions[0].session_id()).expect("permit");
        let reservation = permit.reserve(2).expect("reservation");
        assert_eq!(
            reservation.commit(3),
            Err(FilesystemJobError::AccountingViolation {
                granted: 2,
                observed: 3
            })
        );
        assert_eq!(job.usage().expect("usage").read_bytes, 2);
        assert_eq!(
            permit.reserve(1).expect_err("terminal violation"),
            FilesystemJobError::AccountingViolation {
                granted: 2,
                observed: 3
            }
        );
        drop(permit);
        drop(sessions);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn finish_rejects_active_work_and_cancel_wakes_a_real_waiter() {
        let (root, sessions) = sessions(2);
        let job = FilesystemJobCoordinator::new(limits()).expect("job");
        let mut first_permit = job.begin_read(sessions[0].session_id()).expect("first");
        let held = first_permit.reserve(4).expect("held");
        assert!(matches!(
            job.finish(),
            Err(FilesystemJobError::InFlight { .. })
        ));
        let waiting_job = job.clone();
        let second_id = sessions[1].session_id();
        let waiter = thread::spawn(move || {
            let mut permit = waiting_job.begin_read(second_id).expect("second permit");
            permit.reserve(1).map(|_| ())
        });
        while job.usage().expect("usage").waiting_reservations == 0 {
            thread::yield_now();
        }
        job.cancel().expect("cancel");
        assert_eq!(
            waiter.join().expect("waiter"),
            Err(FilesystemJobError::Cancelled)
        );
        drop(held);
        drop(first_permit);
        drop(sessions);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn directory_probe_records_the_extra_entry_and_stops_the_job() {
        let (root, sessions) = sessions(1);
        let job = FilesystemJobCoordinator::new(limits()).expect("job");
        let mut permit = job
            .begin_directory_read(sessions[0].session_id())
            .expect("directory permit");
        permit
            .reserve_entry()
            .expect("first")
            .commit(1)
            .expect("first commit");
        permit
            .reserve_entry()
            .expect("second")
            .commit(1)
            .expect("second commit");
        let probe = permit.reserve_entry().expect("probe");
        assert!(probe.is_probe());
        assert_eq!(
            probe.commit(1),
            Err(FilesystemJobError::Limit(
                FilesystemJobLimit::DirectoryEntries { limit: 2 }
            ))
        );
        assert_eq!(job.usage().expect("usage").directory_probe_entries, 1);
        drop(permit);
        drop(sessions);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn cancellation_remains_the_terminal_cause_when_late_work_commits() {
        let (root, sessions) = sessions(1);
        let job = FilesystemJobCoordinator::new(limits()).expect("job");
        let mut permit = job.begin_read(sessions[0].id()).expect("permit");
        permit
            .reserve(4)
            .expect("normal")
            .commit(4)
            .expect("normal commit");
        let probe = permit.reserve(1).expect("probe");
        job.cancel().expect("cancel");
        assert_eq!(probe.commit(1), Err(FilesystemJobError::Cancelled));
        assert_eq!(job.cancel(), Ok(()));
        assert!(matches!(
            job.begin_read(sessions[0].id()),
            Err(FilesystemJobError::Cancelled)
        ));
        drop(permit);
        drop(sessions);
        fs::remove_dir_all(root).expect("cleanup");
    }
}
