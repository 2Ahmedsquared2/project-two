//! Thread pinning, memory locking, per-thread resource usage.
//!
//! All three are Linux-only (`sched_setaffinity`, `mlockall`,
//! `getrusage(RUSAGE_THREAD)`). On any other OS they refuse with an
//! "unsupported on this OS" result instead of succeeding as a no-op, so a
//! warm gate can never report a step that did nothing as passed.

use std::fmt;
use std::io;

/// Pinning failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PinError {
    /// The core is not in this process's allowed set.
    #[error("core {0} is not available to this process")]
    NoSuchCore(usize),
    /// `sched_setaffinity` refused.
    #[error("could not pin to core {0}")]
    Refused(usize),
    /// Not Linux: there is no hard thread-to-core pinning to use.
    #[error("pinning a thread to a core is unsupported on this OS ({0}); Linux only")]
    Unsupported(&'static str),
}

/// Pins the calling thread to `core` (an id from the process's allowed set).
#[cfg(target_os = "linux")]
pub fn pin_current_thread(core: usize) -> Result<(), PinError> {
    let ids = core_affinity::get_core_ids().unwrap_or_default();
    let id = ids
        .into_iter()
        .find(|c| c.id == core)
        .ok_or(PinError::NoSuchCore(core))?;
    if core_affinity::set_for_current(id) {
        Ok(())
    } else {
        Err(PinError::Refused(core))
    }
}

/// Pins the calling thread to `core`: unsupported on this OS.
#[cfg(not(target_os = "linux"))]
pub fn pin_current_thread(_core: usize) -> Result<(), PinError> {
    Err(PinError::Unsupported(std::env::consts::OS))
}

/// What `mlockall(MCL_CURRENT | MCL_FUTURE)` did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LockOutcome {
    /// All current and future pages are locked.
    Locked,
    /// Refused, with the reason (`ENOMEM`: `RLIMIT_MEMLOCK` too low; `EPERM`:
    /// no privilege; or "unsupported on this OS").
    Refused(String),
}

impl fmt::Display for LockOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LockOutcome::Locked => f.write_str("mlockall(current|future) locked"),
            LockOutcome::Refused(e) => write!(f, "mlockall(current|future) refused: {e}"),
        }
    }
}

/// Locks all current and future pages of the process.
///
/// Only when `RLIMIT_MEMLOCK` is unlimited: with a finite limit, a successful
/// `MCL_FUTURE` makes every later allocation past the limit fail (seen on the
/// dev box, 8 MiB limit: the process aborted on its next allocations). So a
/// finite limit is reported as refused without calling `mlockall`. A refused
/// call is undone with `munlockall`. On a non-Linux OS: refused, unsupported.
#[must_use]
#[cfg(target_os = "linux")]
pub fn lock_memory() -> LockOutcome {
    use nix::sys::resource::{getrlimit, Resource, RLIM_INFINITY};
    use rustix::mm::{mlockall, MlockAllFlags};
    match getrlimit(Resource::RLIMIT_MEMLOCK) {
        Ok((soft, _)) if soft != RLIM_INFINITY => {
            return LockOutcome::Refused(format!(
                "RLIMIT_MEMLOCK is {soft} bytes, not unlimited (MCL_FUTURE would make later allocations fail)"
            ));
        }
        Ok(_) => {}
        Err(e) => return LockOutcome::Refused(format!("getrlimit(RLIMIT_MEMLOCK): {e}")),
    }
    match mlockall(MlockAllFlags::CURRENT | MlockAllFlags::FUTURE) {
        Ok(()) => LockOutcome::Locked,
        Err(e) => {
            let _ = unlock_memory();
            LockOutcome::Refused(e.to_string())
        }
    }
}

/// Locks all current and future pages: unsupported on this OS, so refused.
#[must_use]
#[cfg(not(target_os = "linux"))]
pub fn lock_memory() -> LockOutcome {
    LockOutcome::Refused(format!(
        "mlockall is unsupported on this OS ({}); Linux only",
        std::env::consts::OS
    ))
}

#[cfg(not(target_os = "linux"))]
fn unsupported(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        format!(
            "{what} is unsupported on this OS ({}); Linux only",
            std::env::consts::OS
        ),
    )
}

/// `munlockall()`: unlocks every page and clears `MCL_FUTURE`.
pub fn unlock_memory() -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        rustix::mm::munlockall().map_err(io::Error::from)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(unsupported("munlockall"))
    }
}

/// `getrusage(RUSAGE_THREAD)` counters of the calling thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThreadUsage {
    minor: i64,
    major: i64,
    voluntary: i64,
    involuntary: i64,
}

/// Change between two [`ThreadUsage`] reads on the same thread.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UsageDelta {
    /// Minor page faults.
    pub minor_faults: i64,
    /// Major page faults.
    pub major_faults: i64,
    /// Voluntary context switches.
    pub voluntary_switches: i64,
    /// Involuntary context switches (preemptions).
    pub involuntary_switches: i64,
}

impl ThreadUsage {
    /// Reads the calling thread's counters. On a non-Linux OS an
    /// `ErrorKind::Unsupported` error, never zeros.
    pub fn now() -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        {
            use nix::sys::resource::{getrusage, UsageWho};
            let u = getrusage(UsageWho::RUSAGE_THREAD).map_err(io::Error::from)?;
            Ok(Self {
                minor: u.minor_page_faults(),
                major: u.major_page_faults(),
                voluntary: u.voluntary_context_switches(),
                involuntary: u.involuntary_context_switches(),
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(unsupported("getrusage(RUSAGE_THREAD)"))
        }
    }

    /// `self - earlier`.
    #[must_use]
    pub fn since(&self, earlier: &ThreadUsage) -> UsageDelta {
        UsageDelta {
            minor_faults: self.minor - earlier.minor,
            major_faults: self.major - earlier.major,
            voluntary_switches: self.voluntary - earlier.voluntary,
            involuntary_switches: self.involuntary - earlier.involuntary,
        }
    }
}
