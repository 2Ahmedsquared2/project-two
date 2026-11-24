//! How the reader waits for new bytes between polls.

use std::path::Path;
use std::time::{Duration, Instant};

/// A way to wait for the node to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detect {
    /// Do not wait: spin and read again. Burns one core.
    BusyPoll,
    /// Sleep a fixed interval between reads.
    Poll,
    /// Sleep until the kernel reports a write (Linux only).
    Inotify,
}

impl Detect {
    /// Parses a `--detect` value: `busy-poll`, `poll` or `inotify`.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let _ = s;
        None
    }

    /// The `--detect` spelling.
    #[must_use]
    pub fn name(self) -> &'static str {
        "unimplemented"
    }
}

/// What one [`Waiter::wait`] learned.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Wake {
    /// A directory changed: look for a newer file.
    pub rescan: bool,
}

/// Waits between polls in the live detect mode.
#[derive(Debug)]
pub struct Waiter {
    mode: Detect,
}

impl Waiter {
    /// A waiter for `requested`, or for the mode it falls back to, with the
    /// reason for the fallback. `poll` is the sleep of [`Detect::Poll`];
    /// `cpus` is how many CPUs this process may run on.
    #[must_use]
    pub fn new(requested: Detect, poll: Duration, cpus: usize) -> (Self, Option<String>) {
        let _ = (poll, cpus);
        (Self { mode: requested }, None)
    }

    /// The mode in use now.
    #[must_use]
    pub fn mode(&self) -> Detect {
        self.mode
    }

    /// Tells the waiter which file is followed now (after any poll that may
    /// have switched files). Returns a note if the mode had to change.
    pub fn follow(&mut self, root: &Path, current: Option<&Path>) -> Option<String> {
        let _ = (root, current);
        None
    }

    /// Waits for new bytes, at most until `until`.
    pub fn wait(&mut self, until: Instant) -> Wake {
        let _ = until;
        Wake::default()
    }
}
