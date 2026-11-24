//! The one clock every stamp in a run comes from.

use std::time::Instant;

/// A monotonic nanosecond clock (`std::time::Instant`, `CLOCK_MONOTONIC` on
/// Linux). It is `Copy`: copies share the epoch, so stamps taken on the
/// feeder thread and on the hot thread subtract correctly.
#[derive(Debug, Clone, Copy)]
pub struct Clock {
    epoch: Instant,
}

impl Clock {
    /// A clock whose zero is now.
    #[must_use]
    pub fn new() -> Self {
        Self {
            epoch: Instant::now(),
        }
    }

    /// Nanoseconds since the clock was made.
    #[inline]
    #[must_use]
    pub fn now_ns(&self) -> u64 {
        // 2^64 ns is 584 years: a run never gets there.
        u64::try_from(self.epoch.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }
}

impl Default for Clock {
    fn default() -> Self {
        Self::new()
    }
}
