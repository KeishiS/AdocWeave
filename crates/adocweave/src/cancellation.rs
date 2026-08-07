//! Deterministic cooperative-cancellation checkpoints shared by core stages.

use std::cmp::Ordering;

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

    pub(crate) const fn cancellation(&self) -> &'a dyn CancellationCheck {
        self.cancellation
    }
}

/// Sorts stably after one cancellation checkpoint.
///
/// The sort itself is not interruptible: the standard stable sort over an
/// in-memory vector finishes fast enough that a host cannot observe a finer
/// cancellation granularity, and an interruptible reimplementation would have
/// to allocate per recursion step.
pub(crate) fn sort_by_cancellable<T>(
    values: &mut [T],
    compare: &mut impl FnMut(&T, &T) -> Ordering,
    checkpoint: &mut CancellationCheckpoint<'_>,
) -> Result<(), ()> {
    if checkpoint.is_cancelled() {
        return Err(());
    }
    values.sort_by(|left, right| compare(left, right));
    Ok(())
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

    #[test]
    fn cancellable_sort_is_stable_and_does_not_require_clone() {
        #[derive(Debug, Eq, PartialEq)]
        struct Item {
            key: u8,
            order: u8,
        }

        let mut values = vec![
            Item { key: 2, order: 0 },
            Item { key: 1, order: 1 },
            Item { key: 2, order: 2 },
            Item { key: 1, order: 3 },
        ];
        sort_by_cancellable(
            &mut values,
            &mut |left, right| left.key.cmp(&right.key),
            &mut CancellationCheckpoint::new(&crate::core::NeverCancel),
        )
        .expect("NeverCancel cannot cancel sorting");

        assert_eq!(
            values,
            vec![
                Item { key: 1, order: 1 },
                Item { key: 1, order: 3 },
                Item { key: 2, order: 0 },
                Item { key: 2, order: 2 },
            ]
        );
    }

    #[test]
    fn cancellable_sort_observes_cancellation_before_sorting() {
        struct AlwaysCancel;

        impl CancellationCheck for AlwaysCancel {
            fn is_cancelled(&self) -> bool {
                true
            }
        }

        let mut values = vec![3_usize, 1, 2];
        let result = sort_by_cancellable(
            &mut values,
            &mut Ord::cmp,
            &mut CancellationCheckpoint::new(&AlwaysCancel),
        );

        assert_eq!(result, Err(()));
        assert_eq!(values, vec![3, 1, 2], "取消時は入力を変更しません");
    }
}
