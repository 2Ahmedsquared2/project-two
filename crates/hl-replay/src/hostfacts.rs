//! The host-facts file `scripts/capture-host.sh` writes, and the check that a
//! run meant for a write-up carries one.

use std::path::{Path, PathBuf};

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
    raw: String,
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
            raw: text.to_owned(),
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

    /// The file's text exactly as it was checked.
    #[must_use]
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// Writes the checked text to `dir/host-facts.txt`.
    ///
    /// The bytes come from memory, never from a second read of the source, so
    /// what is kept is what was checked even if the source changed during the
    /// run or is this very file. They go to a temporary file in `dir` that is
    /// renamed into place, so a reader never sees half a file.
    pub fn write_to(&self, dir: &Path) -> Result<PathBuf, ReplayError> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join("host-facts.txt");
        let tmp = dir.join(format!(".host-facts.txt.{}.tmp", std::process::id()));
        let written = std::fs::write(&tmp, &self.raw).and_then(|()| std::fs::rename(&tmp, &path));
        if let Err(e) = written {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.into());
        }
        Ok(path)
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

    /// The CPUs the kernel command line isolates (`isolcpus=`), in ascending
    /// order and once each, if it names them one by one and every part of the
    /// list was read. A list that selects no CPU gives an empty `Vec`.
    #[must_use]
    pub fn isolated_cpus(&self) -> Option<Vec<usize>> {
        match self.cpu_list("isolcpus")? {
            CpuList::Listed(ranges) => Some(
                (0..MAX_CPUS)
                    .filter(|cpu| ranges.iter().any(|r| r.contains(*cpu)))
                    .collect(),
            ),
            CpuList::All | CpuList::Unreadable => None,
        }
    }

    /// The CPUs a kernel command-line list names, if it is set. One part this
    /// harness cannot read (the kernel's `N` for the last CPU, an unknown
    /// word) or the kernel itself refuses (a CPU number at or past
    /// [`MAX_CPUS`], a stride using more than its group) makes the whole list
    /// unreadable: a partly read list would report a core as outside a list
    /// that holds it. Ranges are kept as ranges, never expanded CPU by CPU.
    ///
    /// A list that selects no CPU (`0-7:0/2`, or `isolcpus=` flags with no
    /// list) is read, as an empty list: the kernel accepts it and sets no
    /// bit (Linux v6.8 `lib/bitmap-str.c`: `bitmap_check_region` refuses only
    /// `off > group_len`, and `bitmap_set_region` sets `min(.., off)` bits).
    ///
    /// Empty parts are skipped and a key with nothing after it is an empty
    /// list, as in the kernel: `bitmap_parselist` calls `bitmap_find_region`
    /// before each region, which steps over any run of commas and ends the
    /// list with success at the terminator, so `2,,3`, `,2,3` and `2,3,` name
    /// CPUs 2 and 3 and an empty string names none (v6.8 `lib/bitmap-str.c`;
    /// `nohz_full=` and `isolcpus=` reach it through `housekeeping_setup` in
    /// `kernel/sched/isolation.c`, `rcu_nocbs=` through `rcu_nocb_setup` in
    /// `kernel/rcu/tree_nocb.h`, each by `cpulist_parse`).
    fn cpu_list(&self, key: &str) -> Option<CpuList> {
        let mut ranges = Vec::new();
        let mut all = false;
        // `isolcpus=` alone takes flags, and only before its list: the kernel
        // takes them while the value starts with a letter and hands the rest
        // to `cpulist_parse` whole (v6.8 `housekeeping_isolcpus_setup`), so
        // an empty part or a CPU ends them.
        let mut flags = key == "isolcpus";
        for part in self.cmdline_value(key)?.split(',') {
            if flags && ISOLCPUS_FLAGS.contains(&part) {
                continue;
            }
            flags = false;
            if part.is_empty() {
                continue;
            }
            if part.eq_ignore_ascii_case("all") {
                all = true;
                continue;
            }
            match CpuRange::parse(part) {
                Some(range) => ranges.push(range),
                None => return Some(CpuList::Unreadable),
            }
        }
        Some(if all {
            CpuList::All
        } else {
            CpuList::Listed(ranges)
        })
    }

    /// A parsed kernel command-line value the script wrote as `key: value`.
    /// A key set with nothing after it (`nohz_full=`) is written `key: `, and
    /// read as the empty value with or without that trailing space: a
    /// section's body is stored without its trailing whitespace.
    fn cmdline_value(&self, key: &str) -> Option<&str> {
        self.section("kernel-cmdline")?
            .lines()
            .find_map(|l| {
                let value = l.strip_prefix(key)?.strip_prefix(':')?;
                if value.is_empty() {
                    Some(value)
                } else {
                    value.strip_prefix(' ')
                }
            })
            .filter(|v| *v != "not set")
    }

    /// Warnings for the pinned cores a kernel CPU list leaves out. `lacking`
    /// is what such a core is not, for the line written when the list is not
    /// set at all.
    fn list_warnings(&self, key: &str, lacking: &str, pinned: &[(&str, usize)]) -> Vec<String> {
        let raw = self.cmdline_value(key).unwrap_or_default();
        match self.cpu_list(key) {
            _ if pinned.is_empty() => Vec::new(),
            Some(CpuList::All) => Vec::new(),
            Some(CpuList::Listed(listed)) => pinned
                .iter()
                .filter(|(_, core)| !listed.iter().any(|r| r.contains(*core)))
                .map(|(role, core)| format!("{role} core {core} is not in {key} ({raw})"))
                .collect(),
            Some(CpuList::Unreadable) => vec![format!(
                "{key} ({raw}) is not a CPU list this harness can read: pinned cores not checked against it"
            )],
            None => {
                let (cores, verb) = name_cores(pinned);
                vec![format!("{key} is not set: {cores} {verb} not {lacking}")]
            }
        }
    }

    /// Warnings about RCU callback offload for cores whose `nohz_full`
    /// membership is unknown because that list was not read: a core outside
    /// `rcu_nocbs` may still be offloaded through `nohz_full`, so the line
    /// says undetermined and never "not offloaded".
    fn undetermined_offload_warnings(&self, pinned: &[(&str, usize)]) -> Vec<String> {
        let tickless = self.cmdline_value("nohz_full").unwrap_or_default();
        let raw = self.cmdline_value("rcu_nocbs").unwrap_or_default();
        match self.cpu_list("rcu_nocbs") {
            Some(CpuList::Listed(listed)) => pinned
                .iter()
                .filter(|(_, core)| !listed.iter().any(|r| r.contains(*core)))
                .map(|(role, core)| {
                    format!(
                        "{role} core {core} is not in rcu_nocbs ({raw}) and nohz_full ({tickless}) could not be parsed: whether it is offloaded from RCU callbacks is undetermined"
                    )
                })
                .collect(),
            None if !pinned.is_empty() => {
                let (cores, verb) = name_cores(pinned);
                vec![format!(
                    "rcu_nocbs is not set and nohz_full ({tickless}) could not be parsed: whether {cores} {verb} offloaded from RCU callbacks is undetermined"
                )]
            }
            // `all` offloads every core; an unreadable rcu_nocbs gets its own line.
            _ => self.list_warnings("rcu_nocbs", "offloaded from RCU callbacks", pinned),
        }
    }

    /// What a reader must weigh about the cores a run pinned to: a core
    /// outside `isolcpus`, `nohz_full` or `rcu_nocbs`, a pinned CPU 0 that
    /// `nohz_full` names, or a hot core that shares its physical core.
    /// Warnings only; none of this stops a run.
    fn core_warnings(&self, feeder: Option<usize>, hot: Option<usize>) -> Vec<String> {
        let pinned: Vec<(&str, usize)> = [("feeder", feeder), ("hot", hot)]
            .into_iter()
            .filter_map(|(role, core)| Some((role, core?)))
            .collect();
        let mut warnings = self.list_warnings("isolcpus", "isolated", &pinned);
        warnings.extend(self.list_warnings("nohz_full", "tickless", &pinned));
        let tickless = self.cpu_list("nohz_full");
        // "The boot CPU will be forced outside the range to maintain the
        // timekeeping" (Linux v6.8 kernel-parameters.txt, nohz_full=). The
        // capture does not record which CPU booted; it is normally CPU 0.
        let names_cpu_0 = match &tickless {
            Some(CpuList::All) => true,
            Some(CpuList::Listed(listed)) => listed.iter().any(|r| r.contains(0)),
            Some(CpuList::Unreadable) | None => false,
        };
        if names_cpu_0 {
            let raw = self.cmdline_value("nohz_full").unwrap_or_default();
            warnings.extend(pinned.iter().filter(|(_, core)| *core == 0).map(|(role, _)| {
                format!(
                    "{role} core 0 is in nohz_full ({raw}), but the kernel forces the boot CPU (normally CPU 0) outside nohz_full: it may still take the tick"
                )
            }));
        }
        if matches!(tickless, Some(CpuList::Unreadable)) {
            warnings.extend(self.undetermined_offload_warnings(&pinned));
        } else {
            // The kernel offloads RCU callbacks on every nohz_full CPU, as if
            // it were in rcu_nocbs too, so only the other cores are checked.
            let not_offloaded: Vec<(&str, usize)> = pinned
                .iter()
                .copied()
                .filter(|(_, core)| match &tickless {
                    Some(CpuList::All) => false,
                    Some(CpuList::Listed(listed)) => !listed.iter().any(|r| r.contains(*core)),
                    Some(CpuList::Unreadable) | None => true,
                })
                .collect();
            warnings.extend(self.list_warnings(
                "rcu_nocbs",
                "offloaded from RCU callbacks",
                &not_offloaded,
            ));
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

/// The flags `isolcpus=` takes before its CPU list (Linux v6.8
/// `Documentation/admin-guide/kernel-parameters.txt`).
const ISOLCPUS_FLAGS: [&str; 3] = ["nohz", "domain", "managed_irq"];

/// The CPUs a kernel command-line CPU list names.
enum CpuList {
    /// The word `all`.
    All,
    /// The CPUs in any of these ranges.
    Listed(Vec<CpuRange>),
    /// Set, with a part this harness cannot read.
    Unreadable,
}

/// One more than the largest CPU number a list may name: the widest CPU mask
/// an x86-64 kernel can be built with (Linux v6.8 `arch/x86/Kconfig`,
/// `NR_CPUS_RANGE_END`, 8192 with `CPUMASK_OFFSTACK`; arm64 stops at 4096).
/// The kernel refuses a list naming a CPU at or past its mask width (-ERANGE,
/// v6.8 `lib/bitmap-str.c`, `bitmap_check_region`).
const MAX_CPUS: usize = 8192;

/// One part of a CPU list: `first`, `first-last` or `first-last:used/group`
/// (Linux v6.8 `Documentation/admin-guide/kernel-parameters.rst`, "cpu
/// lists"). The range is split into groups of `group` CPUs and names the
/// first `used` of each.
struct CpuRange {
    first: usize,
    last: usize,
    used: usize,
    group: usize,
}

impl CpuRange {
    /// Reads one part, or `None` for a part this harness cannot read or the
    /// kernel refuses: `bitmap_check_region` returns -EINVAL for a descending
    /// range, a group of 0 or `used > group`, and -ERANGE for a CPU past the
    /// mask.
    fn parse(part: &str) -> Option<Self> {
        let (range, stride) = match part.split_once(':') {
            None => (part, None),
            Some((range, stride)) => {
                let (used, group) = stride.split_once('/')?;
                (range, Some((used.parse().ok()?, group.parse().ok()?)))
            }
        };
        let (first, last) = range.split_once('-').unwrap_or((range, range));
        let (first, last) = (first.parse::<usize>().ok()?, last.parse::<usize>().ok()?);
        // A plain range is one group, all of it used.
        let (used, group): (usize, usize) = stride.unwrap_or((1, 1));
        (first <= last && last < MAX_CPUS && group > 0 && used <= group).then_some(Self {
            first,
            last,
            used,
            group,
        })
    }

    fn contains(&self, cpu: usize) -> bool {
        (self.first..=self.last).contains(&cpu) && (cpu - self.first) % self.group < self.used
    }
}

/// "feeder core 2 and hot core 3" with the verb that agrees with it.
fn name_cores(pinned: &[(&str, usize)]) -> (String, &'static str) {
    let cores: Vec<String> = pinned
        .iter()
        .map(|(role, core)| format!("{role} core {core}"))
        .collect();
    let verb = if cores.len() == 1 { "is" } else { "are" };
    (cores.join(" and "), verb)
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
