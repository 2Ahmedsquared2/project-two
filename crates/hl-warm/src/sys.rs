//! Thread pinning, memory locking, per-thread resource usage.

use std::fmt;

use nix::sys::resource::{getrusage, UsageWho};

/// Pinning failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PinError {
    /// The core is not in this process's allowed set.
    #[error("core {0} is not available to this process")]
    NoSuchCore(usize),
    /// `sched_setaffinity` refused.
    #[error("could not pin to core {0}")]
    Refused(usize),
}

/// Pins the calling thread to `core` (an id from the process's allowed set).
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

/// What `mlockall(MCL_CURRENT | MCL_FUTURE)` did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LockOutcome {
    /// All current and future pages are locked.
    Locked,
    /// Refused, with the errno text (`ENOMEM`: `RLIMIT_MEMLOCK` too low; `EPERM`: no privilege).
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
#[must_use]
pub fn lock_memory() -> LockOutcome {
    use rustix::mm::{mlockall, MlockAllFlags};
    match mlockall(MlockAllFlags::CURRENT | MlockAllFlags::FUTURE) {
        Ok(()) => LockOutcome::Locked,
        Err(e) => LockOutcome::Refused(e.to_string()),
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
    /// Reads the calling thread's counters.
    pub fn now() -> nix::Result<Self> {
        let u = getrusage(UsageWho::RUSAGE_THREAD)?;
        Ok(Self {
            minor: u.minor_page_faults(),
            major: u.major_page_faults(),
            voluntary: u.voluntary_context_switches(),
            involuntary: u.involuntary_context_switches(),
        })
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
