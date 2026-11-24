//! Stopping on a signal.
//!
//! The first `SIGTERM` or `SIGINT` asks the reader to stop: the handler only
//! stores to two atomics (no work in the handler), and the reader's loop
//! reads one of them per iteration. The run then ends the way a
//! `--duration-s` run does: the writer writes what the ring holds and the
//! counters are printed.
//!
//! A second signal ends the process at once from the handler (`_exit`, which
//! is async-signal-safe) with status 128 plus the signal number: nothing
//! more is written and no counters are printed, so that run is invalid. It
//! is the way out when the writer cannot finish, for example on a disk that
//! has stopped accepting writes.
//!
//! The first signal only sets the flag the reader's loop watches. Once that
//! loop has ended for another reason (the `--duration-s` deadline) nothing
//! reads the flag any more, so a first signal during the writer's final
//! write is not acted on or reported; the next one is still the second and
//! exits.
//!
//! `SIGKILL` cannot be handled and `SIGHUP` is not: both end the process the
//! default way, losing what the ring holds and the counters.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use signal_hook::consts::{SIGINT, SIGTERM};

/// The signals that stop a run.
const STOP_SIGNALS: [i32; 2] = [SIGTERM, SIGINT];

/// Whether a stop signal has arrived, and which.
#[derive(Debug, Clone)]
pub struct Stop {
    requested: Arc<AtomicBool>,
    signal: Arc<AtomicUsize>,
}

impl Stop {
    /// Installs the handlers for `SIGTERM` and `SIGINT`. Until this returns,
    /// those signals end the process the default way.
    pub fn install() -> std::io::Result<Self> {
        let stop = Self {
            requested: Arc::new(AtomicBool::new(false)),
            signal: Arc::new(AtomicUsize::new(0)),
        };
        for sig in STOP_SIGNALS {
            // Handlers run in the order registered. First: if a stop was
            // already requested, this is the second signal, so exit now.
            signal_hook::flag::register_conditional_shutdown(
                sig,
                128_i32.saturating_add(sig),
                Arc::clone(&stop.requested),
            )?;
            // Then record which signal it was, before the flag the reader
            // watches, so whoever sees the flag also sees the number.
            signal_hook::flag::register_usize(
                sig,
                Arc::clone(&stop.signal),
                usize::try_from(sig).unwrap_or(usize::MAX),
            )?;
            signal_hook::flag::register(sig, Arc::clone(&stop.requested))?;
        }
        Ok(stop)
    }

    /// True once a stop signal has arrived. One atomic load: no system call,
    /// lock or allocation, so the reader checks it on every iteration.
    #[inline]
    #[must_use]
    pub fn requested(&self) -> bool {
        self.requested.load(Ordering::Relaxed)
    }

    /// The number of the signal that asked for the stop, if one did.
    #[must_use]
    pub fn signal(&self) -> Option<i32> {
        if !self.requested() {
            return None;
        }
        i32::try_from(self.signal.load(Ordering::Relaxed))
            .ok()
            .filter(|&n| n != 0)
    }
}
