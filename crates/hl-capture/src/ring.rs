//! The hand-off from the read/stamp thread to the disk writer.

use std::io::Write;
use std::time::Duration;

use hl_latency::Observation;

use crate::tail::ObservationSink;

/// Default ring size, in records.
pub const DEFAULT_RING_RECORDS: usize = 65_536;

/// The reader's end of the ring.
#[derive(Debug)]
pub struct RingSink {
    dropped: u64,
}

/// The writer's end of the ring.
#[derive(Debug)]
pub struct RingSource {}

/// A bounded single-producer single-consumer ring of `capacity` records.
#[must_use]
pub fn ring(capacity: usize) -> (RingSink, RingSource) {
    let _ = capacity;
    (RingSink { dropped: 0 }, RingSource {})
}

impl ObservationSink for RingSink {
    fn observe(&mut self, obs: Observation) {
        let _ = obs;
    }
}

impl RingSink {
    /// Records dropped because the ring was full.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// True when the writer's end is gone, so nothing pushed will be written.
    #[must_use]
    pub fn abandoned(&self) -> bool {
        false
    }
}

impl RingSource {
    /// The oldest record, if any.
    pub fn pop(&mut self) -> Option<Observation> {
        None
    }

    /// Writes every record now in the ring to `out` as JSON lines and
    /// flushes if it wrote any. Returns how many it wrote.
    pub fn write_available<W: Write>(&mut self, out: &mut W) -> std::io::Result<u64> {
        let _ = out;
        Ok(0)
    }

    /// The writer thread's loop: writes records as they arrive, sleeping
    /// `idle` whenever the ring is empty, until the [`RingSink`] is dropped
    /// and the ring is empty. Returns how many records it wrote.
    pub fn run<W: Write>(self, out: W, idle: Duration) -> std::io::Result<u64> {
        let _ = (out, idle);
        Ok(0)
    }
}
