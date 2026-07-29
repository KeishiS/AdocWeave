//! Deterministic cooperative-cancellation checkpoints shared by core stages.

use crate::core::CancellationCheck;

pub(crate) const CHECKPOINT_INTERVAL: usize = 256;

pub(crate) struct CancellationCheckpoint<'a> {
    cancellation: &'a dyn CancellationCheck,
    until_check: usize,
}

impl<'a> CancellationCheckpoint<'a> {
    pub(crate) const fn new(cancellation: &'a dyn CancellationCheck) -> Self {
        Self {
            cancellation,
            until_check: 0,
        }
    }

    pub(crate) fn is_cancelled(&mut self) -> bool {
        if self.until_check == 0 {
            self.until_check = CHECKPOINT_INTERVAL - 1;
            self.cancellation.is_cancelled()
        } else {
            self.until_check -= 1;
            false
        }
    }

    pub(crate) fn is_cancelled_now(&self) -> bool {
        self.cancellation.is_cancelled()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct CountChecks(AtomicUsize);

    impl CancellationCheck for CountChecks {
        fn is_cancelled(&self) -> bool {
            self.0.fetch_add(1, Ordering::Relaxed);
            false
        }
    }

    #[test]
    fn checkpoint_interval_is_deterministic() {
        let cancellation = CountChecks(AtomicUsize::new(0));
        let mut checkpoint = CancellationCheckpoint::new(&cancellation);
        for _ in 0..=CHECKPOINT_INTERVAL {
            assert!(!checkpoint.is_cancelled());
        }
        assert_eq!(cancellation.0.load(Ordering::Relaxed), 2);
    }
}
