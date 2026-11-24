//! The hand-off from the read/stamp thread to the disk writer: a bounded
//! single-producer single-consumer ring of fixed-size records.
//!
//! # Full-ring policy: drop the newest, count it, never block
//!
//! [`RingSink::observe`] is one wait-free push. When the ring is full the
//! record being pushed is dropped and counted in [`RingSink::dropped`]; the
//! reader does not wait, retry or slow down. A reader that waited for the
//! disk would stamp every later line late, and a late stamp looks exactly
//! like a slow node: it corrupts the measurement without a trace. A dropped
//! record is only a missing sample, and it is counted, so a run with a
//! non-zero drop count is known to be incomplete and should be thrown away.
//! Records already in the ring are never overwritten, so what reaches the
//! file is always in order.

use std::io::Write;
use std::time::Duration;

use hl_latency::Observation;

use crate::tail::ObservationSink;

/// Default ring size, in records (32 bytes each: 2 MiB).
pub const DEFAULT_RING_RECORDS: usize = 65_536;

/// Most records the writer thread writes before it calls its hook again
/// (see [`RingSource::run_with`]).
pub const WRITE_CHUNK: usize = 1024;

/// The reader's end of the ring.
#[derive(Debug)]
pub struct RingSink {
    tx: rtrb::Producer<Observation>,
    dropped: u64,
}

/// The writer's end of the ring.
#[derive(Debug)]
pub struct RingSource {
    rx: rtrb::Consumer<Observation>,
}

/// A bounded single-producer single-consumer ring of `capacity` records,
/// allocated once, here.
#[must_use]
pub fn ring(capacity: usize) -> (RingSink, RingSource) {
    let (tx, rx) = rtrb::RingBuffer::new(capacity);
    (RingSink { tx, dropped: 0 }, RingSource { rx })
}

impl ObservationSink for RingSink {
    /// Pushes the record, or drops and counts it when the ring is full.
    /// Never blocks, never allocates, makes no system call.
    fn observe(&mut self, obs: Observation) {
        if self.tx.push(obs).is_err() {
            self.dropped += 1;
        }
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
        self.tx.is_abandoned()
    }
}

impl RingSource {
    /// The oldest record, if any.
    pub fn pop(&mut self) -> Option<Observation> {
        self.rx.pop().ok()
    }

    /// Writes every record now in the ring to `out` as JSON lines and
    /// flushes if it wrote any. Returns how many it wrote.
    pub fn write_available<W: Write>(&mut self, out: &mut W) -> std::io::Result<u64> {
        self.write_up_to(out, usize::MAX)
    }

    /// Like [`Self::write_available`], stopping after `max` records.
    pub fn write_up_to<W: Write>(&mut self, out: &mut W, max: usize) -> std::io::Result<u64> {
        let mut n = 0;
        let mut left = max;
        while left > 0 {
            let Some(obs) = self.pop() else { break };
            left -= 1;
            serde_json::to_writer(&mut *out, &obs)?;
            out.write_all(b"\n")?;
            n += 1;
        }
        if n > 0 {
            // Per batch, so a crash loses little.
            out.flush()?;
        }
        Ok(n)
    }

    /// The writer thread's loop: writes records as they arrive, sleeping
    /// `idle` whenever the ring is empty (the reader never wakes it: that
    /// would be a system call on the reader's path), until the [`RingSink`]
    /// is dropped and the ring is empty. Returns how many records it wrote.
    /// On a write error it returns at once, which drops this end and shows
    /// the reader [`RingSink::abandoned`].
    pub fn run<W: Write>(self, out: W, idle: Duration) -> std::io::Result<u64> {
        self.run_with(out, idle, || Ok(()))
    }

    /// [`Self::run`], calling `between` before every chunk of at most
    /// [`WRITE_CHUNK`] records and before every idle sleep, so other work
    /// that belongs on this thread (the look for a newer hourly file) waits
    /// for one chunk's write at most, not for a whole backlog. An error
    /// from `between` ends the loop like a write error.
    pub fn run_with<W: Write>(
        mut self,
        mut out: W,
        idle: Duration,
        mut between: impl FnMut() -> std::io::Result<()>,
    ) -> std::io::Result<u64> {
        let mut total = 0;
        loop {
            // Not implemented yet (red): the hook is never called.
            let _ = &mut between;
            let n = self.write_available(&mut out)?;
            total += n;
            if n > 0 {
                continue;
            }
            if self.rx.is_abandoned() {
                // The sink is gone: whatever it pushed last is visible now.
                total += self.write_available(&mut out)?;
                return Ok(total);
            }
            std::thread::sleep(idle);
        }
    }
}
