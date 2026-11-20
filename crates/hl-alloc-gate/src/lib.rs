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
//! Stub: nothing is counted yet.

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

/// Runs `f` and reports what it allocated on this thread.
pub fn measure<R>(f: impl FnOnce() -> R) -> (R, Allocs) {
    (f(), Allocs::default())
}

/// Runs `tick` `warmup` times unmeasured, then `ticks` times measured one by
/// one. `tick` receives the tick index, counting from 0 across warmup and
/// measured ticks, so a test can feed it different input each time.
pub fn count_ticks(warmup: u64, ticks: u64, mut tick: impl FnMut(u64)) -> TickReport {
    for i in 0..warmup.saturating_add(ticks) {
        tick(i);
    }
    TickReport {
        ticks,
        ..TickReport::default()
    }
}

/// The gate: panics, naming `what`, if any tick after warmup allocates.
///
/// # Panics
///
/// When a measured tick allocates. The message gives the first offending
/// tick, the worst tick and the totals.
pub fn assert_no_alloc_after_warmup(what: &str, warmup: u64, ticks: u64, tick: impl FnMut(u64)) {
    let _ = (what, count_ticks(warmup, ticks, tick));
}
