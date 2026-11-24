//! The host-facts file `scripts/capture-host.sh` writes, and the check that a
//! run meant for a write-up carries one.

use std::path::Path;

use crate::ReplayError;

/// A parsed host-facts file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostFacts {
    captured_utc: String,
    kernel: String,
    sections: Vec<(String, String)>,
}

impl HostFacts {
    /// The sections the capture script always writes and the harness requires.
    pub const REQUIRED: [&'static str; 16] = [
        "kernel-cmdline",
        "lscpu",
        "numa",
        "cstates",
        "governor",
        "clocksource",
        "tsc",
        "ena-irq-affinity",
        "irqbalance",
        "busy-poll-sysctls",
        "ethtool-c",
        "ethtool-k",
        "ethtool-l",
        "thp",
        "swappiness",
        "chrony",
    ];
    /// The section an operator appends by hand; the harness never requires it.
    pub const MANUAL_RTT: &'static str = "rtt-manual";

    /// Parses a host-facts file.
    pub fn parse(_text: &str) -> Result<Self, ReplayError> {
        Err(ReplayError::HostFacts("not implemented".to_owned()))
    }

    /// Reads and parses a host-facts file.
    pub fn load(_path: &Path) -> Result<Self, ReplayError> {
        Err(ReplayError::HostFacts("not implemented".to_owned()))
    }

    /// When the facts were captured (UTC), as the script wrote it.
    #[must_use]
    pub fn captured_utc(&self) -> &str {
        &self.captured_utc
    }

    /// The kernel release the facts were captured on.
    #[must_use]
    pub fn kernel(&self) -> &str {
        &self.kernel
    }

    /// A section's body.
    #[must_use]
    pub fn section(&self, _name: &str) -> Option<&str> {
        None
    }

    /// Required sections holding at least one fact that could not be read.
    #[must_use]
    pub fn unavailable(&self) -> Vec<&'static str> {
        Vec::new()
    }

    /// Whether the operator appended the manual RTT section.
    #[must_use]
    pub fn has_manual_rtt(&self) -> bool {
        false
    }

    /// Fails unless the facts were captured on the running kernel.
    pub fn check_kernel(&self, _running: &str) -> Result<(), ReplayError> {
        Err(ReplayError::HostFacts("not implemented".to_owned()))
    }
}

/// What a report says about host facts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum HostFactsNote {
    /// Nothing was written to disk, so no facts were required.
    #[default]
    NotRequired,
    /// `--allow-missing-host-facts`: a local run without facts.
    Waived,
    /// A capture was given and passed the check.
    Captured {
        /// The file.
        path: String,
        /// When it was captured (UTC).
        captured_utc: String,
        /// Sections holding at least one unreadable fact.
        unavailable: Vec<String>,
        /// Whether the manual RTT section is there.
        manual_rtt: bool,
    },
}

impl HostFactsNote {
    /// The note for a checked capture.
    #[must_use]
    pub fn captured(_path: &Path, _facts: &HostFacts) -> Self {
        Self::NotRequired
    }
}
