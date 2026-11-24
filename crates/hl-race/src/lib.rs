//! Ingest racer: several sources deliver the same keyed stream; the first
//! copy of each key is released exactly once, in key order; every later copy
//! is counted, timed against the winner and compared byte for byte.
//!
//! # Keys
//!
//! The racer orders by a dense `u64` sequence number. For hl-node output the
//! sequence is the block height (`block_number`, read with
//! [`hl_latency::scan_node_header`]; see [`node_line_seq`]). Sources without a
//! height (the public `WebSocket`) carry only the block time, which is sparse
//! (blocks ~70 ms apart), so they cannot drive ordered release; they race at
//! block granularity in `hl-latency`'s pairing instead. What was measured
//! about both keys is in `docs/proofs/race.md`.
//!
//! # Guarantees (checked by `formal/race/Race.tla` with TLC, and tested)
//!
//! - Each sequence number is released at most once.
//! - Releases are in strictly increasing sequence order; a missing number is
//!   released only after it arrives (gap fill from any source) or after the
//!   caller gives up on it with [`Racer::skip_missing`], which is counted.
//! - A copy whose bytes differ from the held or released copy of the same
//!   number returns [`Offer::Divergent`] and is counted per source: the racer
//!   never silently keeps the first.
//!
//! # Hot path
//!
//! [`Racer::new`] allocates everything: `slots * max_payload` bytes of payload
//! store and a fixed histogram per source. [`Racer::offer`] and
//! [`Racer::pop`] never allocate (gated by `tests/alloc.rs`).

use std::sync::OnceLock;
use std::time::Instant;

use serde::Serialize;

mod hist;
mod window;

pub use hist::LagHistogram;
pub use window::{classify, Position};

/// Nanoseconds on this process's monotonic clock, counted from the first
/// call. Sources stamp each read with it, so all arrivals share one clock.
#[must_use]
pub fn mono_ns() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    let start = *START.get_or_init(Instant::now);
    u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// The dedupe key of one hl-node block-info line: its block height. `None`
/// for a line [`hl_latency::scan_node_header`] does not recognise.
#[must_use]
pub fn node_line_seq(line: hl_latency::CompleteLine<'_>) -> Option<u64> {
    hl_latency::scan_node_header(line).map(|(_, height)| height)
}

/// The part of an hl-node block-info line that two nodes write identically:
/// everything from `,"block_time"` on. `local_time`, the first field, is the
/// writing node's own clock, so two nodes' copies of one block differ there
/// and comparing whole lines would raise a false divergence alarm. `None`
/// for a line that does not start with hl-node's exact prefix (see
/// [`hl_latency::scan_node_header`]).
#[must_use]
pub fn node_line_body(line: hl_latency::CompleteLine<'_>) -> Option<&[u8]> {
    let b = line.as_bytes();
    let rest = b.strip_prefix(br#"{"local_time":""#)?;
    let close = rest.iter().position(|&c| c == b'"' || c == b'\\')?;
    let tail = &rest[close..];
    tail.starts_with(br#"","block_time":""#).then(|| &tail[1..])
}

/// One arrival from a source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Item<'a> {
    /// Dense sequence number (block height).
    pub seq: u64,
    /// The bytes the source delivered for it.
    pub payload: &'a [u8],
    /// [`mono_ns`] when the source read it.
    pub t_recv_ns: u64,
}

/// A source of keyed items. Implementations stamp `t_recv_ns` at read time.
pub trait Source {
    /// The next item, or `None` when nothing is ready now.
    fn poll_item(&mut self) -> Option<Item<'_>>;
}

/// A scripted source for tests and simulations: replays a fixed list.
#[derive(Debug, Clone, Default)]
pub struct SimSource {
    items: Vec<(u64, Vec<u8>, u64)>,
    at: usize,
}

impl SimSource {
    /// A source that yields `items` (`seq`, payload, `t_recv_ns`) in order.
    #[must_use]
    pub fn new(items: Vec<(u64, Vec<u8>, u64)>) -> Self {
        Self { items, at: 0 }
    }
}

impl Source for SimSource {
    fn poll_item(&mut self) -> Option<Item<'_>> {
        let (seq, payload, t) = self.items.get(self.at)?;
        self.at += 1;
        Some(Item {
            seq: *seq,
            payload,
            t_recv_ns: *t,
        })
    }
}

/// What the racer did with one offered copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "a Divergent result is an alarm and must be handled"]
pub enum Offer {
    /// First copy of this number: held for release; the source wins it.
    Won,
    /// Same bytes as the first copy; `lag_ns` behind the winner's stamp
    /// (0 when this copy was stamped earlier but offered later).
    Duplicate {
        /// This copy's stamp minus the winner's, saturating at 0.
        lag_ns: u64,
    },
    /// Different bytes from the first copy of the same number. ALARM.
    Divergent,
    /// Behind the window and its slot reused: cannot be compared; counted.
    Late,
    /// Too far ahead of the next number to release; dropped and counted.
    TooFar,
    /// Larger than the configured per-item payload limit; dropped, counted.
    TooLarge,
    /// The source index is out of range; nothing recorded.
    UnknownSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotState {
    Empty,
    Held,
    Released,
}

#[derive(Debug, Clone, Copy)]
struct Slot {
    state: SlotState,
    seq: u64,
    winner: usize,
    t_first_ns: u64,
    len: usize,
}

const EMPTY: Slot = Slot {
    state: SlotState::Empty,
    seq: 0,
    winner: 0,
    t_first_ns: 0,
    len: 0,
};

/// Counters for one source.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SourceStats {
    /// Numbers this source delivered first.
    pub wins: u64,
    /// Copies identical to an earlier copy.
    pub duplicates: u64,
    /// Copies whose bytes differed from the first copy (alarms).
    pub divergences: u64,
    /// Copies behind the window that could not be compared.
    pub late: u64,
    /// Copies too far ahead of the window.
    pub too_far: u64,
    /// Copies over the payload limit.
    pub too_large: u64,
    /// Duplicates stamped earlier than the winner but offered after it.
    pub stamped_earlier: u64,
    /// Lag behind the winner, nanoseconds, for each duplicate.
    pub lag: LagHistogram,
}

/// One released item, borrowed from the racer's store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Released<'a> {
    /// Its sequence number.
    pub seq: u64,
    /// Index of the source whose copy arrived first.
    pub winner: usize,
    /// The winner's stamp.
    pub t_first_ns: u64,
    /// The winning bytes.
    pub payload: &'a [u8],
}

/// Snapshot of all counters, for a JSON report.
#[derive(Debug, Clone, Serialize)]
pub struct Report<'a> {
    /// Next number to release.
    pub next: u64,
    /// Numbers released.
    pub released: u64,
    /// Numbers given up with [`Racer::skip_missing`].
    pub gaps: u64,
    /// `u64::MAX` was released or skipped: the stream is finished.
    pub exhausted: bool,
    /// Per source, by index.
    pub sources: &'a [SourceStats],
}

/// The racer. See the crate docs for its guarantees.
#[derive(Debug)]
pub struct Racer {
    slots: Vec<Slot>,
    store: Vec<u8>,
    max_payload: usize,
    next: u64,
    /// Set once `u64::MAX` is released or skipped; `next` stays at
    /// `u64::MAX` and every number counts as behind.
    exhausted: bool,
    released: u64,
    gaps: u64,
    stats: Vec<SourceStats>,
}

impl Racer {
    /// A racer for `sources` sources, holding up to `slots` numbers at or
    /// ahead of the next release, each at most `max_payload` bytes; starts at
    /// sequence `start`. `slots` must be at least 1 (0 is raised to 1).
    #[must_use]
    pub fn new(sources: usize, slots: usize, max_payload: usize, start: u64) -> Self {
        let slots = slots.max(1);
        Self {
            slots: vec![EMPTY; slots],
            store: vec![0; slots.saturating_mul(max_payload)],
            max_payload,
            next: start,
            exhausted: false,
            released: 0,
            gaps: 0,
            stats: vec![SourceStats::default(); sources],
        }
    }

    fn window(&self) -> u64 {
        u64::try_from(self.slots.len()).unwrap_or(u64::MAX)
    }

    fn bytes(&self, slot: usize) -> &[u8] {
        let s = &self.slots[slot];
        let at = slot * self.max_payload;
        &self.store[at..at + s.len]
    }

    /// Offers one copy from source `source`.
    ///
    /// Driver contract: call [`Racer::pop`] until it returns `None` before
    /// the next `offer`. Otherwise a key `window` ahead of a releasable one
    /// is dropped as [`Offer::TooFar`], and if no other source resends it
    /// the stream stalls (TLC counter-example, `formal/race/Race.tla`).
    pub fn offer(&mut self, source: usize, item: Item<'_>) -> Offer {
        if source >= self.stats.len() {
            return Offer::UnknownSource;
        }
        if item.payload.len() > self.max_payload {
            self.stats[source].too_large += 1;
            return Offer::TooLarge;
        }
        let Some(pos) = classify(self.next, item.seq, self.window()) else {
            return Offer::UnknownSource; // unreachable: the window is >= 1
        };
        let slot = match pos {
            Position::TooFar => {
                self.stats[source].too_far += 1;
                return Offer::TooFar;
            }
            // Once exhausted, nothing is ahead: every number is behind.
            Position::Ahead { slot } | Position::Behind { slot } => slot,
        };
        let behind = self.exhausted || matches!(pos, Position::Behind { .. });
        let s = self.slots[slot];
        let holds_seq = s.state != SlotState::Empty && s.seq == item.seq;
        if !holds_seq {
            if behind {
                self.stats[source].late += 1;
                return Offer::Late;
            }
            // Ahead and not held: first copy. Any old occupant is a released
            // number at least `window` behind; it is overwritten.
            let at = slot * self.max_payload;
            self.store[at..at + item.payload.len()].copy_from_slice(item.payload);
            self.slots[slot] = Slot {
                state: SlotState::Held,
                seq: item.seq,
                winner: source,
                t_first_ns: item.t_recv_ns,
                len: item.payload.len(),
            };
            self.stats[source].wins += 1;
            return Offer::Won;
        }
        if self.bytes(slot) != item.payload {
            self.stats[source].divergences += 1;
            return Offer::Divergent;
        }
        let st = &mut self.stats[source];
        st.duplicates += 1;
        let lag_ns = item.t_recv_ns.saturating_sub(s.t_first_ns);
        if item.t_recv_ns < s.t_first_ns {
            st.stamped_earlier += 1;
        }
        st.lag.record(lag_ns);
        Offer::Duplicate { lag_ns }
    }

    /// Releases the next number if its first copy has arrived.
    pub fn pop(&mut self) -> Option<Released<'_>> {
        if self.exhausted {
            return None;
        }
        let Position::Ahead { slot } = classify(self.next, self.next, self.window())? else {
            return None;
        };
        let s = self.slots[slot];
        if s.state != SlotState::Held || s.seq != self.next {
            return None;
        }
        self.slots[slot].state = SlotState::Released;
        self.released += 1;
        self.advance();
        Some(Released {
            seq: s.seq,
            winner: s.winner,
            t_first_ns: s.t_first_ns,
            payload: self.bytes(slot),
        })
    }

    /// Gives up on the next number when it has not arrived (a stall the
    /// caller timed out, or window pressure), counting a gap. Returns the
    /// skipped number, or `None` when it is held (pop it instead).
    pub fn skip_missing(&mut self) -> Option<u64> {
        if self.exhausted {
            return None;
        }
        let Some(Position::Ahead { slot }) = classify(self.next, self.next, self.window()) else {
            return None;
        };
        let s = self.slots[slot];
        if s.state == SlotState::Held && s.seq == self.next {
            return None;
        }
        let skipped = self.next;
        self.gaps += 1;
        self.advance();
        Some(skipped)
    }

    /// Moves past `next`; at `u64::MAX` there is no successor, so the racer
    /// becomes exhausted instead (`next` stays `u64::MAX`).
    fn advance(&mut self) {
        match self.next.checked_add(1) {
            Some(n) => self.next = n,
            None => self.exhausted = true,
        }
    }

    /// True once `u64::MAX` has been released or skipped: nothing further
    /// can be released, and every later copy is a duplicate, divergent or
    /// late.
    #[must_use]
    pub fn is_exhausted(&self) -> bool {
        self.exhausted
    }

    /// The next number to release.
    #[must_use]
    pub fn next_seq(&self) -> u64 {
        self.next
    }

    /// Per-source counters, by source index.
    #[must_use]
    pub fn stats(&self) -> &[SourceStats] {
        &self.stats
    }

    /// All counters, ready to serialise.
    #[must_use]
    pub fn report(&self) -> Report<'_> {
        Report {
            next: self.next,
            released: self.released,
            gaps: self.gaps,
            exhausted: self.exhausted,
            sources: &self.stats,
        }
    }
}

/// Polls each source in turn, offering every item and releasing what can be
/// released into `sink`, until all sources return `None`. Divergences go to
/// `alarm(source, seq)`. A test and simulation driver; a live driver polls
/// in its own loop.
pub fn drain_round_robin(
    racer: &mut Racer,
    sources: &mut [&mut dyn Source],
    mut sink: impl FnMut(Released<'_>),
    mut alarm: impl FnMut(usize, u64),
) {
    loop {
        let mut any = false;
        for (i, src) in sources.iter_mut().enumerate() {
            if let Some(item) = src.poll_item() {
                any = true;
                let seq = item.seq;
                if racer.offer(i, item) == Offer::Divergent {
                    alarm(i, seq);
                }
                while let Some(r) = racer.pop() {
                    sink(r);
                }
            }
        }
        if !any {
            return;
        }
    }
}
