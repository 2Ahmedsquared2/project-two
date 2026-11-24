//! Warm-up before serving or measuring.
//!
//! A cold process pays for page faults, lazy statics, cold caches, untrained
//! branch predictors and connection handshakes on its first events. This crate
//! makes the node earn its way to "warm" and keeps every sample taken before
//! that out of reported numbers:
//!
//! 1. [`pin_current_thread`] pins a hot thread to a given core.
//! 2. [`lock_memory`] calls `mlockall(MCL_CURRENT | MCL_FUTURE)` and reports
//!    the outcome (it may be refused by `RLIMIT_MEMLOCK`).
//! 3. [`Arena`] maps hot-path memory up front, backed by `MAP_HUGETLB` when the
//!    host has reserved huge pages, else advised for transparent huge pages,
//!    else normal pages, records which one it got, and writes every page.
//! 4. The caller pushes recorded and synthetic events through its whole hot
//!    path (parse -> execute -> serve) N times, and opens its sources.
//! 5. [`WarmGate`] stays closed until every named step passed, and then until
//!    a window of both time and events has elapsed. [`GatedHistogram`] drops
//!    every sample recorded while the gate was closed.
//!
//! [`ThreadUsage`] reads `getrusage(RUSAGE_THREAD)` so a test can show 0
//! faults on the hot thread over a measured window.
//!
//! Platforms: pinning, `mlockall`, huge pages and `getrusage(RUSAGE_THREAD)`
//! are Linux-only. Elsewhere pinning and locking refuse with "unsupported on
//! this OS", [`ThreadUsage::now`] returns an `Unsupported` error and an
//! [`Arena`] records [`Backing::Normal`]; nothing reports a step it did not do.
//!
//! No `unsafe` here: `rustix`, `nix` and `memmap2` hold it.

mod arena;
mod gate;
mod sys;

pub use arena::{Arena, Backing};
pub use gate::{CoreError, GateConfig, GateCore, GateError, GatedHistogram, Phase, WarmGate};
pub use sys::{
    lock_memory, pin_current_thread, unlock_memory, LockOutcome, PinError, ThreadUsage, UsageDelta,
};
