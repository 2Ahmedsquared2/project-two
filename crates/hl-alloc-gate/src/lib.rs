//! The zero-allocation gate for hot-path code.
//!
//! A tick on the hot path (one line parsed, one book update, one order built)
//! must not touch the heap once its buffers are warm: an allocation is where
//! the p99 goes. This crate turns that rule into a test that fails.
//!
//! Use it as a dev-dependency only. Linking it replaces the global allocator
//! of the test binary with a counting wrapper around the system allocator
//! (from the `allocation-counter` crate, which holds the one `unsafe impl
//! GlobalAlloc`; this workspace forbids `unsafe`).
//!
//! Counts are per thread: only allocations made by the thread that calls
//! [`measure`] or [`count_ticks`] are seen. A tick that hands work to another
//! thread must be gated on that thread.
//!
//! ```
//! let mut buf: Vec<u8> = Vec::new();
//! hl_alloc_gate::assert_no_alloc_after_warmup("copy into a reused buffer", 8, 1000, |_| {
//!     buf.clear();
//!     buf.extend_from_slice(b"same size every tick");
//! });
//! ```

/// Heap activity of one measured region, on the calling thread.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Allocs {
    /// Calls to the allocator that returned new memory. A `realloc` (a `Vec`
    /// growing) counts as one.
    pub count: u64,
    /// Bytes requested by those calls.
    pub bytes: u64,
}

/// Heap activity of a run of ticks after warmup.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TickReport {
    /// Ticks measured (warmup excluded).
    pub ticks: u64,
    /// Measured ticks that allocated at least once.
    pub allocating_ticks: u64,
    /// Total over the measured ticks.
    pub total: Allocs,
    /// The first measured tick that allocated: its index (0 = first tick
    /// after warmup) and what it allocated.
    pub first: Option<(u64, Allocs)>,
    /// The measured tick with the most allocations: its index and count.
    pub worst: Option<(u64, Allocs)>,
}

/// Deepest supported nesting of [`measure`] calls on one thread.
pub const MAX_NESTING: u32 = 32;

/// Runs `f` and reports what it allocated on this thread.
pub fn measure<R>(f: impl FnOnce() -> R) -> (R, Allocs) {
    let mut out = None;
    let info = allocation_counter::measure(|| out = Some(f()));
    let allocs = Allocs {
        count: info.count_total,
        bytes: info.bytes_total,
    };
    match out {
        Some(r) => (r, allocs),
        // `allocation_counter::measure` calls its closure exactly once.
        None => unreachable!("measured closure did not run"),
    }
}

/// Runs `tick` `warmup` times unmeasured, then `ticks` times measured one by
/// one. `tick` receives the tick index, counting from 0 across warmup and
/// measured ticks, so a test can feed it different input each time.
pub fn count_ticks(warmup: u64, ticks: u64, mut tick: impl FnMut(u64)) -> TickReport {
    for i in 0..warmup {
        tick(i);
    }
    let mut report = TickReport {
        ticks,
        ..TickReport::default()
    };
    for n in 0..ticks {
        let ((), a) = measure(|| tick(warmup.saturating_add(n)));
        if a.count == 0 {
            continue;
        }
        report.allocating_ticks += 1;
        report.total.count += a.count;
        report.total.bytes += a.bytes;
        if report.first.is_none() {
            report.first = Some((n, a));
        }
        if report.worst.is_none_or(|(_, w)| a.count > w.count) {
            report.worst = Some((n, a));
        }
    }
    report
}

/// The gate: panics, naming `what`, if any tick after warmup allocates.
///
/// # Panics
///
/// When a measured tick allocates, or when `ticks` is 0 (a gate that measured
/// nothing must not pass). The message gives the first offending tick, the
/// worst tick and the totals.
pub fn assert_no_alloc_after_warmup(what: &str, warmup: u64, ticks: u64, tick: impl FnMut(u64)) {
    assert!(ticks > 0, "{what}: zero ticks measured");
    let r = count_ticks(warmup, ticks, tick);
    if let (Some((first, fa)), Some((worst, wa))) = (r.first, r.worst) {
        panic!(
            "{what}: {} of {} ticks allocated after {warmup} warmup ticks \
             ({} allocations, {} bytes in total); \
             first at tick {first} ({} allocations, {} bytes); \
             worst at tick {worst} ({} allocations, {} bytes)",
            r.allocating_ticks,
            r.ticks,
            r.total.count,
            r.total.bytes,
            fa.count,
            fa.bytes,
            wa.count,
            wa.bytes,
        );
    }
}
