//! The per-line core every arm shares, and the busy-poll engine.

use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::sink::ensure_loopback_peer;
use crate::{Clock, Handle, LatencyHist, LineBuf, ReplayError};

/// The histograms one arm fills. All in nanoseconds on one [`Clock`].
#[derive(Debug, Default, Clone)]
pub struct Hists {
    /// `t1 - t0`: line seen by the reader to parsed.
    pub parse: LatencyHist,
    /// `t2 - t0`: line seen by the reader to bytes handed to the socket.
    pub e2e: LatencyHist,
    /// `t0 - feed`: the feeder's stamp just before it wrote the line, to the
    /// reader seeing it. This is where the arms differ most: it holds the
    /// runtime's (or the file poller's) wake-up delay. Empty when the lines
    /// were not fed with stamps.
    pub detect: LatencyHist,
    /// `t2 - feed`: the two above together, per line.
    pub total: LatencyHist,
    /// `t2 - scheduled`: from when the line was due to be sent to bytes
    /// handed to the socket. The headline for comparing arms.
    pub sched_total: LatencyHist,
    /// Lines re-parsed by the strict parser off the timed path.
    pub validated: u64,
    /// Of those, lines a header-scan parser accepted and the strict parse
    /// refused. Must be 0.
    pub scan_strict_mismatches: u64,
    /// Lines a header-scan parser could not scan and parsed strictly.
    pub scan_fallbacks: u64,
}

impl Hists {
    /// Adds another round's samples.
    pub fn merge(&mut self, other: &Self) {
        self.parse.merge(&other.parse);
        self.e2e.merge(&other.e2e);
        self.detect.merge(&other.detect);
        self.total.merge(&other.total);
        self.sched_total.merge(&other.sched_total);
        self.validated += other.validated;
        self.scan_strict_mismatches += other.scan_strict_mismatches;
        self.scan_fallbacks += other.scan_fallbacks;
    }
}

/// What [`Core::step`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// No complete line is buffered.
    NoLine,
    /// A line was handled and [`Core::out`] holds an order: write it, then
    /// call [`Core::sent`].
    Order,
    /// A line was handled and produced no order.
    NoOrder,
}

/// Reader buffer, pipeline, stamps and histograms: everything an engine
/// needs except the I/O itself. An engine reads into [`Core::spare_mut`],
/// stamps `t0`, then loops [`Core::step`] / write / [`Core::sent`].
#[derive(Debug)]
pub struct Core<H> {
    handler: H,
    clock: Clock,
    warmup: u64,
    seen: u64,
    lines: LineBuf,
    line_capacity: usize,
    out: Vec<u8>,
    hists: Hists,
    feed_stamps: Option<Arc<[AtomicU64]>>,
    schedule_stamps: Option<Arc<[AtomicU64]>>,
    t0: u64,
    t1: u64,
    validate_every: u64,
}

impl<H: Handle> Core<H> {
    /// A core that discards the first `warmup` lines' timings and can buffer
    /// `line_capacity` bytes of input.
    pub fn new(handler: H, clock: Clock, warmup: u64, line_capacity: usize) -> Self {
        Self {
            handler,
            clock,
            warmup,
            seen: 0,
            lines: LineBuf::with_capacity(line_capacity),
            line_capacity,
            out: Vec::with_capacity(1024),
            hists: Hists::default(),
            feed_stamps: None,
            schedule_stamps: None,
            t0: 0,
            t1: 0,
            validate_every: 0,
        }
    }

    /// Gives the core the feeder's per-line stamps (index = line number), so
    /// it can record [`Hists::detect`] and [`Hists::total`].
    #[must_use]
    pub fn with_feed_stamps(mut self, stamps: Arc<[AtomicU64]>) -> Self {
        self.feed_stamps = Some(stamps);
        self
    }

    /// Re-parses every `every`-th line (0 = never) with the strict parser,
    /// [`hl_latency::parse_node_line`], **after** `t2` has been stamped and
    /// the latencies recorded, so it is off the timed path. A line the
    /// pipeline's parser accepted and the strict parse refuses is counted in
    /// [`Hists::scan_strict_mismatches`]. For header-scan parsers, which can
    /// accept a line whose body is broken. It allocates (the strict parser
    /// does), so the allocation gates run without it.
    #[must_use]
    pub fn with_strict_validation(mut self, every: u64) -> Self {
        self.validate_every = every;
        self
    }

    /// Gives the core the time each line was *scheduled* to be sent (index
    /// = line number), so it can record [`Hists::sched_total`]. Measuring
    /// from the schedule rather than from the write keeps a late feeder's
    /// delay in the numbers (no coordinated omission).
    #[must_use]
    pub fn with_schedule_stamps(mut self, stamps: Arc<[AtomicU64]>) -> Self {
        self.schedule_stamps = Some(stamps);
        self
    }

    /// Lends the reader's storage; see [`LineBuf::take_storage`].
    pub fn take_reader(&mut self) -> Result<(Box<[u8]>, usize), ReplayError> {
        self.lines.take_storage()
    }

    /// Returns the reader's storage; see [`LineBuf::restore_storage`].
    pub fn restore_reader(&mut self, storage: Box<[u8]>, filled: usize) {
        self.lines.restore_storage(storage, filled);
    }

    /// The run's clock.
    pub fn clock(&self) -> Clock {
        self.clock
    }

    /// The reader buffer's size.
    pub fn line_capacity(&self) -> usize {
        self.line_capacity
    }

    /// Free reader space to read into; see [`LineBuf::spare_mut`].
    pub fn spare_mut(&mut self) -> Result<&mut [u8], ReplayError> {
        self.lines.spare_mut()
    }

    /// Marks `n` bytes of [`Core::spare_mut`] as read.
    pub fn advance(&mut self, n: usize) {
        self.lines.advance(n);
    }

    /// Copies bytes into the reader buffer.
    pub fn push(&mut self, bytes: &[u8]) -> Result<(), ReplayError> {
        self.lines.push(bytes)
    }

    /// Handles the next buffered line, if a whole one is there. `t0_ns` is
    /// when the read that delivered it returned.
    #[inline]
    pub fn step(&mut self, t0_ns: u64) -> Result<Step, ReplayError> {
        let Some(line) = self.lines.next_complete_line() else {
            return Ok(Step::NoLine);
        };
        let outcome = self
            .handler
            .on_line(line, t0_ns, &self.clock, &mut self.out)?;
        self.t0 = t0_ns;
        self.t1 = outcome.t1_ns;
        if outcome.order {
            Ok(Step::Order)
        } else {
            // Nothing is sent, so there is no t2: the line is counted, not timed.
            self.seen += 1;
            Ok(Step::NoOrder)
        }
    }

    /// The order bytes to write after [`Step::Order`].
    pub fn out(&self) -> &[u8] {
        &self.out
    }

    /// Takes the order buffer, for engines that must own what they write.
    /// Give it back with [`Core::restore_out`] before the next step.
    pub fn take_out(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }

    /// Returns the buffer taken with [`Core::take_out`].
    pub fn restore_out(&mut self, out: Vec<u8>) {
        self.out = out;
    }

    /// Call when the write of [`Core::out`] has returned: stamps `t2` and,
    /// past warmup, records the line's latencies.
    #[inline]
    pub fn sent(&mut self) {
        let t2 = self.clock.now_ns();
        let index = self.seen;
        self.seen += 1;
        if self.validate_every > 0 && index.is_multiple_of(self.validate_every) {
            self.validate_last_line();
        }
        if index < self.warmup {
            return;
        }
        self.hists.parse.record(self.t1.saturating_sub(self.t0));
        self.hists.e2e.record(t2.saturating_sub(self.t0));
        let stamp = |stamps: &Option<Arc<[AtomicU64]>>| {
            stamps
                .as_ref()
                .and_then(|s| s.get(usize::try_from(index).ok()?))
                .map(|a| a.load(Ordering::Relaxed))
        };
        if let Some(fed) = stamp(&self.feed_stamps) {
            self.hists.detect.record(self.t0.saturating_sub(fed));
            self.hists.total.record(t2.saturating_sub(fed));
        }
        if let Some(due) = stamp(&self.schedule_stamps) {
            self.hists.sched_total.record(t2.saturating_sub(due));
        }
    }

    /// The strict check of the line just sent. Cold on purpose: it runs after
    /// `t2` and must not be inlined into the timed path.
    #[cold]
    #[inline(never)]
    fn validate_last_line(&mut self) {
        self.hists.validated += 1;
        let strict_ok = std::str::from_utf8(self.lines.last_line())
            .is_ok_and(|l| hl_latency::parse_node_line(l, 0).is_ok());
        if !strict_ok {
            self.hists.scan_strict_mismatches += 1;
        }
    }

    /// The histograms so far.
    pub fn hists(&self) -> &Hists {
        &self.hists
    }

    /// Consumes the core, keeping the histograms.
    pub fn into_hists(self) -> Hists {
        self.hists
    }

    /// Lines handled so far, warmup included.
    pub fn lines_seen(&self) -> u64 {
        self.seen
    }
}

/// What one [`BusyTick::poll`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Polled {
    /// Nothing new, or only part of a line.
    Idle,
    /// This many lines were read, handled and their orders written.
    Lines(u64),
    /// The source was closed by its writer.
    Eof,
}

/// The busy-poll engine: one non-blocking `read`, then every complete line
/// goes through the pipeline and out to the sink before `poll` returns. The
/// caller spins on [`Polled::Idle`]. Nothing here blocks, locks or allocates
/// (given a handler that does not).
#[derive(Debug)]
pub struct BusyTick<R, H> {
    source: R,
    sink: TcpStream,
    core: Core<H>,
    /// A tailed file reports end-of-file as "nothing new yet".
    tail: bool,
    refused: Option<ReplayError>,
}

impl<R: Read, H: Handle> BusyTick<R, H> {
    /// Reads a stream `source` (which the caller has set non-blocking) and
    /// writes orders to `sink`. A `sink` whose peer is not loopback makes
    /// every [`BusyTick::poll`] fail without writing a byte.
    pub fn new(source: R, sink: TcpStream, core: Core<H>) -> Self {
        Self::build(source, sink, core, false)
    }

    /// Like [`BusyTick::new`] for a file that is still being appended to.
    pub fn tail(source: R, sink: TcpStream, core: Core<H>) -> Self {
        Self::build(source, sink, core, true)
    }

    fn build(source: R, sink: TcpStream, core: Core<H>, tail: bool) -> Self {
        let refused = ensure_loopback_peer(sink.peer_addr())
            .and_then(|()| Ok(sink.set_nonblocking(true)?))
            .err();
        Self {
            source,
            sink,
            core,
            tail,
            refused,
        }
    }

    /// One read and everything that follows from it.
    #[inline]
    pub fn poll(&mut self) -> Result<Polled, ReplayError> {
        if let Some(e) = self.refused.take() {
            // Stay refused: put an equivalent error back for any later call.
            self.refused = Some(ReplayError::NotLoopback("send stage sink".to_owned()));
            return Err(e);
        }
        let n = match self.source.read(self.core.spare_mut()?) {
            Ok(0) => return Ok(if self.tail { Polled::Idle } else { Polled::Eof }),
            Ok(n) => n,
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {
                return Ok(Polled::Idle)
            }
            Err(e) => return Err(e.into()),
        };
        let t0 = self.core.clock().now_ns();
        self.core.advance(n);
        let mut lines = 0;
        loop {
            match self.core.step(t0)? {
                Step::NoLine => break,
                Step::Order => {
                    write_all_spin(&mut self.sink, self.core.out())?;
                    self.core.sent();
                    lines += 1;
                }
                Step::NoOrder => lines += 1,
            }
        }
        Ok(if lines == 0 {
            Polled::Idle
        } else {
            Polled::Lines(lines)
        })
    }

    /// The core, for progress and histograms.
    pub fn core(&self) -> &Core<H> {
        &self.core
    }

    /// Consumes the engine (closing the sink connection), keeping the core.
    pub fn into_core(self) -> Core<H> {
        self.core
    }
}

/// `write_all` on a non-blocking socket: spin while the send buffer is full.
#[inline]
fn write_all_spin(sink: &mut TcpStream, mut bytes: &[u8]) -> Result<(), ReplayError> {
    while !bytes.is_empty() {
        match sink.write(bytes) {
            Ok(0) => return Err(std::io::Error::from(ErrorKind::WriteZero).into()),
            Ok(n) => bytes = &bytes[n..],
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {
                std::hint::spin_loop();
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
