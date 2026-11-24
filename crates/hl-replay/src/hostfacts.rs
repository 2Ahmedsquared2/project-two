//! The host-facts file `scripts/capture-host.sh` writes, and the check that a
//! run meant for a write-up carries one.

use std::path::Path;

use crate::ReplayError;

fn bad(why: String) -> ReplayError {
    ReplayError::HostFacts(why)
}

/// A parsed host-facts file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostFacts {
    captured_utc: String,
    kernel: String,
    sections: Vec<(String, String)>,
}

impl HostFacts {
    /// The sections the capture script always writes and the harness requires.
    pub const REQUIRED: [&'static str; 18] = [
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
        "hugepages-mlock",
        "smt",
    ];
    /// The section an operator appends by hand; the harness never requires it.
    pub const MANUAL_RTT: &'static str = "rtt-manual";

    /// The first line of every host-facts file.
    pub const MAGIC: &'static str = "hl-host-facts v1";

    /// Parses a host-facts file and checks it holds everything the capture
    /// script writes: the header and every [`Self::REQUIRED`] section,
    /// terminated and not empty. A fact the host could not give is written by
    /// the script as `unavailable (why)`, which is accepted; a section left
    /// out is not.
    pub fn parse(text: &str) -> Result<Self, ReplayError> {
        let mut lines = text.lines();
        if lines.next() != Some(Self::MAGIC) {
            return Err(bad(format!(
                "the first line is not {:?}: not written by scripts/capture-host.sh",
                Self::MAGIC
            )));
        }
        let mut facts = Self {
            captured_utc: String::new(),
            kernel: String::new(),
            sections: Vec::new(),
        };
        facts.read_body(lines)?;
        if facts.captured_utc.is_empty() {
            return Err(bad("no captured-utc line".to_owned()));
        }
        if facts.kernel.is_empty() {
            return Err(bad("no kernel line".to_owned()));
        }
        let missing: Vec<&str> = Self::REQUIRED
            .into_iter()
            .filter(|want| facts.section(want).is_none())
            .collect();
        if !missing.is_empty() {
            return Err(bad(format!(
                "missing section(s): {} (capture again with scripts/capture-host.sh)",
                missing.join(", ")
            )));
        }
        Ok(facts)
    }

    /// Reads the header fields and the sections that follow the first line.
    fn read_body<'a>(&mut self, lines: impl Iterator<Item = &'a str>) -> Result<(), ReplayError> {
        let mut open: Option<(String, String)> = None;
        for line in lines {
            if let Some((name, body)) = open.as_mut() {
                if line.strip_prefix("== end ") == Some(name.as_str()) {
                    if body.trim().is_empty() {
                        return Err(bad(format!("section {name} is empty")));
                    }
                    self.sections
                        .push((std::mem::take(name), body.trim_end().to_owned()));
                    open = None;
                } else if let Some(other) = line.strip_prefix("== begin ") {
                    return Err(bad(format!(
                        "section {name} is not terminated before section {other} begins"
                    )));
                } else {
                    body.push_str(line);
                    body.push('\n');
                }
            } else if let Some(name) = line.strip_prefix("== begin ") {
                if self.section(name).is_some() {
                    return Err(bad(format!("section {name} appears twice")));
                }
                open = Some((name.to_owned(), String::new()));
            } else if let Some(v) = line.strip_prefix("captured-utc: ") {
                v.trim().clone_into(&mut self.captured_utc);
            } else if let Some(v) = line.strip_prefix("kernel: ") {
                v.trim().clone_into(&mut self.kernel);
            }
        }
        match open {
            Some((name, _)) => Err(bad(format!("section {name} is not terminated"))),
            None => Ok(()),
        }
    }

    /// Reads and parses a host-facts file.
    pub fn load(path: &Path) -> Result<Self, ReplayError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| ReplayError::HostFacts(format!("{}: {e}", path.display())))?;
        Self::parse(&text).map_err(|e| match e {
            ReplayError::HostFacts(why) => {
                ReplayError::HostFacts(format!("{}: {why}", path.display()))
            }
            other => other,
        })
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

    /// A section's body, without its trailing newline.
    #[must_use]
    pub fn section(&self, name: &str) -> Option<&str> {
        self.sections
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, body)| body.as_str())
    }

    /// Required sections holding at least one fact the host could not give,
    /// in [`Self::REQUIRED`] order.
    #[must_use]
    pub fn unavailable(&self) -> Vec<&'static str> {
        Self::REQUIRED
            .into_iter()
            .filter(|name| {
                self.section(name)
                    .is_some_and(|body| body.contains("unavailable"))
            })
            .collect()
    }

    /// The CPUs the kernel command line isolates (`isolcpus=`), if any.
    #[must_use]
    pub fn isolated_cpus(&self) -> Option<Vec<usize>> {
        let mut cpus = Vec::new();
        // Flags such as `domain` and `managed_irq` are not CPU numbers.
        for part in self.cmdline_value("isolcpus")?.split(',') {
            let (first, last) = part.split_once('-').unwrap_or((part, part));
            if let (Ok(first), Ok(last)) = (first.parse::<usize>(), last.parse::<usize>()) {
                cpus.extend(first..=last);
            }
        }
        (!cpus.is_empty()).then_some(cpus)
    }

    /// A parsed kernel command-line value the script wrote as `key: value`.
    fn cmdline_value(&self, key: &str) -> Option<&str> {
        self.section("kernel-cmdline")?
            .lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix(": "))
            .filter(|v| *v != "not set")
    }

    /// What a reader must weigh about the cores a run pinned to: a core
    /// outside the isolated set, or a hot core that shares its physical core.
    /// Warnings only; none of this stops a run.
    fn core_warnings(&self, feeder: Option<usize>, hot: Option<usize>) -> Vec<String> {
        let pinned: Vec<(&str, usize)> = [("feeder", feeder), ("hot", hot)]
            .into_iter()
            .filter_map(|(role, core)| Some((role, core?)))
            .collect();
        let mut warnings = Vec::new();
        match self.isolated_cpus() {
            Some(isolated) => {
                let raw = self.cmdline_value("isolcpus").unwrap_or_default();
                for (role, core) in &pinned {
                    if !isolated.contains(core) {
                        warnings.push(format!("{role} core {core} is not in isolcpus ({raw})"));
                    }
                }
            }
            None if !pinned.is_empty() => {
                let cores: Vec<String> = pinned
                    .iter()
                    .map(|(role, core)| format!("{role} core {core}"))
                    .collect();
                let verb = if cores.len() == 1 { "is" } else { "are" };
                warnings.push(format!(
                    "isolcpus is not set: {} {verb} not isolated",
                    cores.join(" and ")
                ));
            }
            None => {}
        }
        if let Some(hot) = hot {
            let prefix = format!("cpu{hot} thread siblings: ");
            let siblings = self
                .section("smt")
                .and_then(|body| body.lines().find_map(|l| l.strip_prefix(prefix.as_str())));
            if let Some(siblings) = siblings {
                if siblings.contains([',', '-']) {
                    warnings.push(format!(
                        "hot core {hot} shares its physical core with another thread (siblings {siblings})"
                    ));
                }
            }
        }
        warnings
    }

    /// Whether the operator appended the manual RTT section. The harness
    /// reports this and never requires or reads it.
    #[must_use]
    pub fn has_manual_rtt(&self) -> bool {
        self.section(Self::MANUAL_RTT).is_some()
    }

    /// Fails unless the facts were captured on the running kernel: the cheap
    /// check that a file was not carried over from another host or boot image.
    pub fn check_kernel(&self, running: &str) -> Result<(), ReplayError> {
        if self.kernel == running {
            Ok(())
        } else {
            Err(ReplayError::HostFacts(format!(
                "captured on kernel {} but this host runs {running}: capture again on this host",
                self.kernel
            )))
        }
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
        /// Things about the host a reader must weigh; never a failure.
        warnings: Vec<String>,
    },
}

impl HostFactsNote {
    /// The note for a checked capture, with warnings about the cores the run
    /// pins its feeder and hot thread to.
    #[must_use]
    pub fn captured(
        path: &Path,
        facts: &HostFacts,
        feeder_core: Option<usize>,
        hot_core: Option<usize>,
    ) -> Self {
        Self::Captured {
            path: path.display().to_string(),
            captured_utc: facts.captured_utc().to_owned(),
            unavailable: facts.unavailable().into_iter().map(str::to_owned).collect(),
            manual_rtt: facts.has_manual_rtt(),
            warnings: facts.core_warnings(feeder_core, hot_core),
        }
    }
}
