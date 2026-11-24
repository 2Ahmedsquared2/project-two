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
//!
//! The writer thread also carries the look for a newer hourly file (see
//! [`crate::Scout`]): [`RingSource::run_with`] calls a hook between chunks.

use std::io::Write;
use std::time::Duration;

use hl_latency::{Observation, Source};

use crate::tail::ObservationSink;

/// Default ring size, in records ([`RECORD_BYTES`] each).
pub const DEFAULT_RING_RECORDS: usize = 65_536;

/// Size of one ring record, in bytes.
pub const RECORD_BYTES: usize = std::mem::size_of::<Record>();

// The reader writes one of these per line: its size is part of the reader's
// cost and of the ring's memory, so it changes only on purpose.
const _: () = assert!(RECORD_BYTES == 40);

const WS: u8 = 1;
const HAS_HEIGHT: u8 = 2;
const HAS_FILE: u8 = 4;

/// An [`Observation`] as the ring holds it: the same fields without the
/// `Option` tags, which are folded into one byte of flags. 40 bytes against
/// 64 for the observation itself.
#[derive(Debug, Clone, Copy)]
struct Record {
    block_time_ms: u64,
    block_number: u64,
    recv_ns: u64,
    line_offset: u64,
    file_date: u32,
    file_hour: u8,
    flags: u8,
}

impl From<Observation> for Record {
    #[inline]
    fn from(o: Observation) -> Self {
        let mut flags = match o.source {
            Source::Node => 0,
            Source::PublicWs => WS,
        };
        if o.block_number.is_some() {
            flags |= HAS_HEIGHT;
        }
        // All three or none: a record with only some is written without
        // any.
        let file = match (o.file_date, o.file_hour, o.line_offset) {
            (Some(date), Some(hour), Some(offset)) => {
                flags |= HAS_FILE;
                (date, hour, offset)
            }
            _ => (0, 0, 0),
        };
        Self {
            block_time_ms: o.block_time_ms,
            block_number: o.block_number.unwrap_or(0),
            recv_ns: o.recv_ns,
            line_offset: file.2,
            file_date: file.0,
            file_hour: file.1,
            flags,
        }
    }
}

impl From<Record> for Observation {
    fn from(r: Record) -> Self {
        let file = r.flags & HAS_FILE != 0;
        Self {
            source: if r.flags & WS == 0 {
                Source::Node
            } else {
                Source::PublicWs
            },
            block_time_ms: r.block_time_ms,
            block_number: (r.flags & HAS_HEIGHT != 0).then_some(r.block_number),
            recv_ns: r.recv_ns,
            file_date: file.then_some(r.file_date),
            file_hour: file.then_some(r.file_hour),
            line_offset: file.then_some(r.line_offset),
        }
    }
}

/// Most records the writer thread writes before it calls its hook again
/// (see [`RingSource::run_with`]).
pub const WRITE_CHUNK: usize = 1024;

/// The reader's end of the ring.
#[derive(Debug)]
pub struct RingSink {
    tx: rtrb::Producer<Record>,
    dropped: u64,
}

/// The writer's end of the ring.
#[derive(Debug)]
pub struct RingSource {
    rx: rtrb::Consumer<Record>,
}

/// A bounded single-producer single-consumer ring of `capacity` records,
/// allocated once, here.
#[must_use]
pub fn ring(capacity: usize) -> (RingSink, RingSource) {
    let (tx, rx) = rtrb::RingBuffer::new(capacity);
    (RingSink { tx, dropped: 0 }, RingSource { rx })
}

/// Longest JSON line a record can make, newline included: the fixed text
/// and every number at its widest come to 201 bytes.
const LINE_MAX: usize = 256;

/// One output line being built on the stack.
struct Line {
    bytes: [u8; LINE_MAX],
    len: usize,
}

impl Line {
    const fn new() -> Self {
        Self {
            bytes: [0; LINE_MAX],
            len: 0,
        }
    }

    #[inline]
    fn text(&mut self, s: &[u8]) {
        let end = self.len.saturating_add(s.len());
        // Never out of room (see `LINE_MAX`); if it were, the line would be
        // cut short and fail to parse, loudly, instead of panicking here.
        if let Some(dst) = self.bytes.get_mut(self.len..end) {
            dst.copy_from_slice(s);
            self.len = end;
        }
    }

    #[inline]
    fn number<I: itoa::Integer>(&mut self, n: I) {
        self.text(itoa::Buffer::new().format(n).as_bytes());
    }

    fn as_bytes(&self) -> &[u8] {
        self.bytes.get(..self.len).unwrap_or_default()
    }
}

impl Record {
    /// The record as its JSON line, byte for byte what `serde_json` writes
    /// for the [`Observation`] plus a newline (a test holds the two equal).
    /// Written by hand because the writer thread does little else: with the
    /// three file fields the generic serialiser took about 75% longer per
    /// record.
    fn encode(&self, line: &mut Line) {
        line.len = 0;
        line.text(if self.flags & WS == 0 {
            br#"{"source":"node","block_time_ms":"#
        } else {
            br#"{"source":"public_ws","block_time_ms":"#
        });
        line.number(self.block_time_ms);
        if self.flags & HAS_HEIGHT != 0 {
            line.text(br#","block_number":"#);
            line.number(self.block_number);
        }
        line.text(br#","recv_ns":"#);
        line.number(self.recv_ns);
        if self.flags & HAS_FILE != 0 {
            line.text(br#","file_date":"#);
            line.number(self.file_date);
            line.text(br#","file_hour":"#);
            line.number(self.file_hour);
            line.text(br#","line_offset":"#);
            line.number(self.line_offset);
        }
        line.text(b"}\n");
    }
}

impl ObservationSink for RingSink {
    /// Pushes the record, or drops and counts it when the ring is full.
    /// Never blocks, never allocates, makes no system call.
    fn observe(&mut self, obs: Observation) {
        if self.tx.push(Record::from(obs)).is_err() {
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
        self.rx.pop().ok().map(Observation::from)
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
        let mut line = Line::new();
        while left > 0 {
            let Ok(record) = self.rx.pop() else { break };
            left -= 1;
            record.encode(&mut line);
            out.write_all(line.as_bytes())?;
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
            between()?;
            let n = self.write_up_to(&mut out, WRITE_CHUNK)?;
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
