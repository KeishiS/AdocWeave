//! Measurement of resource reads and workspace discovery.
//!
//! A meter counts what this crate asked the operating system for while acquiring
//! resources. Byte counts are what a reader handed back at the [`Read`] boundary,
//! which is not the same as disk traffic: a file the operating system served from
//! its own cache still counts every byte, while a file answered from the text
//! cache inside [`LocalTargetSession`] counts none because no read happens.
//!
//! Measurement never rejects an operation. Enforcing an upper bound on a whole
//! analysis job is a separate concern that consumes these counters.
//!
//! [`Read`]: std::io::Read
//! [`LocalTargetSession`]: crate::local_target::LocalTargetSession

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// A shared destination for filesystem measurements.
///
/// Cloning a meter shares its counters rather than copying them, so a draft
/// cloned from a session records into that session's meter. Discarding the draft
/// therefore leaves the work it already performed counted, which is what a
/// per-job upper bound needs: a retry must not become free by throwing away the
/// attempt that preceded it.
///
/// Writing side comparison performed by
/// [`LocalTargetPolicy::candidate_contents_match`] and
/// [`LocalTargetPolicy::replace_candidate_after_recheck`] is deliberately kept
/// out of these counters. Those reads decide whether an already produced result
/// still matches the file on disk, so they belong to applying a change rather
/// than to acquiring the resources an analysis needs.
///
/// [`LocalTargetPolicy::candidate_contents_match`]: crate::local_target::LocalTargetPolicy::candidate_contents_match
/// [`LocalTargetPolicy::replace_candidate_after_recheck`]: crate::local_target::LocalTargetPolicy::replace_candidate_after_recheck
#[derive(Clone, Debug)]
pub(crate) struct FilesystemIoMeter {
    counters: Arc<FilesystemIoCounters>,
}

#[derive(Debug, Default)]
struct FilesystemIoCounters {
    read_operations: AtomicU64,
    read_bytes: AtomicU64,
    directory_read_operations: AtomicU64,
    directory_entries: AtomicU64,
}

impl FilesystemIoMeter {
    /// Creates a meter whose counters no other holder can observe.
    ///
    /// This is the right choice for work that is outside resource acquisition,
    /// such as loading the project configuration file.
    pub(crate) fn detached() -> Self {
        Self {
            counters: Arc::new(FilesystemIoCounters::default()),
        }
    }

    /// Counts one attempt to acquire a resource.
    ///
    /// The attempt is counted before it can fail, so a missing file, a refused
    /// permission and a rejection by the configured limits all count here even
    /// though none of them produces bytes.
    pub(crate) fn observe_read_operation(&self) {
        Self::add(&self.counters.read_operations, 1);
    }

    /// Counts bytes handed back at the [`Read`] boundary.
    ///
    /// This includes the one extra byte read to notice that a resource exceeds
    /// its limit, and bytes already obtained when a read fails part of the way
    /// through or turns out not to be UTF-8.
    ///
    /// [`Read`]: std::io::Read
    pub(crate) fn observe_read_bytes(&self, bytes: usize) {
        Self::add(&self.counters.read_bytes, bytes as u64);
    }

    /// Counts one attempt to enumerate a directory.
    pub(crate) fn observe_directory_read(&self) {
        Self::add(&self.counters.directory_read_operations, 1);
    }

    /// Counts one entry produced by a directory enumeration.
    ///
    /// An entry the operating system reported as an error still counts: the work
    /// of producing it was performed either way.
    pub(crate) fn observe_directory_entry(&self) {
        Self::add(&self.counters.directory_entries, 1);
    }

    /// Reads the counters accumulated so far.
    #[cfg(test)]
    pub(crate) fn usage(&self) -> FilesystemIoUsage {
        FilesystemIoUsage {
            read_operations: self.counters.read_operations.load(Ordering::Relaxed),
            read_bytes: self.counters.read_bytes.load(Ordering::Relaxed),
            directory_read_operations: self
                .counters
                .directory_read_operations
                .load(Ordering::Relaxed),
            directory_entries: self.counters.directory_entries.load(Ordering::Relaxed),
        }
    }

    /// Counters only ever grow and nothing is ordered against them, so relaxed
    /// ordering is enough. Saturating keeps an implausible total from wrapping
    /// back to a small number that would look like almost no work.
    fn add(counter: &AtomicU64, amount: u64) {
        let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            Some(current.saturating_add(amount))
        });
    }
}

/// A snapshot of one meter.
///
/// Only tests read the counters today. The upper bound on a whole analysis job
/// is the intended consumer, and it does not exist yet.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct FilesystemIoUsage {
    pub(crate) read_operations: u64,
    pub(crate) read_bytes: u64,
    pub(crate) directory_read_operations: u64,
    pub(crate) directory_entries: u64,
}

#[cfg(test)]
impl FilesystemIoUsage {
    /// Counters accumulated between `earlier` and this snapshot.
    ///
    /// One meter serves a whole session, so a test that measures a single
    /// operation compares two snapshots rather than reading absolute totals.
    pub(crate) fn since(self, earlier: Self) -> Self {
        Self {
            read_operations: self.read_operations - earlier.read_operations,
            read_bytes: self.read_bytes - earlier.read_bytes,
            directory_read_operations: self.directory_read_operations
                - earlier.directory_read_operations,
            directory_entries: self.directory_entries - earlier.directory_entries,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cloned_meter_shares_its_counters() {
        let meter = FilesystemIoMeter::detached();
        let clone = meter.clone();

        clone.observe_read_operation();
        clone.observe_read_bytes(7);
        clone.observe_directory_read();
        clone.observe_directory_entry();
        drop(clone);

        assert_eq!(
            meter.usage(),
            FilesystemIoUsage {
                read_operations: 1,
                read_bytes: 7,
                directory_read_operations: 1,
                directory_entries: 1,
            }
        );
    }

    #[test]
    fn a_detached_meter_shares_nothing() {
        let meter = FilesystemIoMeter::detached();
        FilesystemIoMeter::detached().observe_read_bytes(9);

        assert_eq!(meter.usage(), FilesystemIoUsage::default());
    }

    #[test]
    fn counters_saturate_instead_of_wrapping() {
        let counter = AtomicU64::new(u64::MAX - 1);

        FilesystemIoMeter::add(&counter, 5);

        assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);
    }
}
