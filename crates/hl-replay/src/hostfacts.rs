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
            // The kernel ignores a refused `isolcpus=`: nothing is isolated.
            CpuList::Refused => Some(Vec::new()),
            CpuList::All | CpuList::Unreadable => None,
        }
    }

    /// The CPUs a kernel command-line list names, if it is set, read the way
    /// the kernel reads it. Source read at Linux v6.8; the lines relied on
    /// are the same in v5.15, v6.1 and v6.12. Nothing here was checked on a
    /// booted host.
    ///
    /// - A list the kernel refuses on any host is [`CpuList::Refused`]: one
    ///   refused part refuses the whole list, since `bitmap_parselist`
    ///   returns at the first error (`lib/bitmap-str.c`; see
    ///   [`Region::parse`] for what is refused). What the kernel then does
    ///   depends on the key: `rcu_nocb_setup` (`kernel/rcu/tree_nocb.h`)
    ///   prints `rcu_nocbs= bad CPU range, all CPUs set` and sets every CPU;
    ///   `housekeeping_setup` (`kernel/sched/isolation.c`) prints
    ///   `nohz_full= or isolcpus= incorrect CPU range` and sets nothing.
    /// - A list with a part only the host can settle (the kernel's `N` for
    ///   the last CPU, `all` with a stride) and no refused part is
    ///   [`CpuList::Unreadable`] as a whole: a partly read list would report
    ///   a core as outside a list that holds it.
    /// - A list that selects no CPU (`0-7:0/2`, `isolcpus=domain,`) is read
    ///   as an empty list: `bitmap_check_region` refuses only
    ///   `off > group_len`, and `bitmap_set_region` sets `min(.., off)` bits.
    /// - Empty parts are skipped and a key with nothing after it is an empty
    ///   list: `bitmap_parselist` calls `bitmap_find_region` before each
    ///   region, which steps over any run of commas and ends the list with
    ///   success at the terminator.
    ///
    /// `nohz_full=` and `isolcpus=` reach `bitmap_parselist` through
    /// `housekeeping_setup`, `rcu_nocbs=` through `rcu_nocb_setup`, each by
    /// `cpulist_parse`. Ranges are kept as ranges, never expanded.
    fn cpu_list(&self, key: &str) -> Option<CpuList> {
        let value = self.cmdline_value(key)?;
        let list = if key == "isolcpus" {
            match isolcpus_list(value) {
                Ok(list) => list,
                Err(unread) => return Some(unread),
            }
        } else {
            value
        };
        let mut ranges = Vec::new();
        let (mut all, mut host_dependent) = (false, false);
        for part in list.split(',').filter(|part| !part.is_empty()) {
            match Region::parse(part) {
                Region::Range(range) => ranges.push(range),
                Region::All => all = true,
                Region::HostDependent => host_dependent = true,
                Region::Refused => return Some(CpuList::Refused),
            }
        }
        Some(if host_dependent {
            CpuList::Unreadable
        } else if all {
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
            Some(CpuList::Refused) if key == "rcu_nocbs" => vec![format!(
                "{key} ({raw}) is a CPU list the kernel refuses: the kernel offloads every CPU from RCU callbacks instead; pinned cores are offloaded"
            )],
            Some(CpuList::Refused) => {
                let (cores, verb) = name_cores(pinned);
                vec![format!(
                    "{key} ({raw}) is a CPU list the kernel refuses and ignores: {cores} {verb} not {lacking}"
                )]
            }
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
            // `all` and a refused list offload every core; a refused or
            // unreadable rcu_nocbs gets its own line.
            _ => self.list_warnings("rcu_nocbs", "offloaded from RCU callbacks", pinned),
        }
    }

    /// What a reader must weigh about the cores a run pinned to (feeder, hot
    /// thread and the `compio-sqpoll` polling thread): a core outside
    /// `isolcpus`, `nohz_full` or `rcu_nocbs`, a pinned CPU 0 that
    /// `nohz_full` names, a hot core that shares its physical core, and
    /// [`Self::sqpoll_warnings`].
    /// Warnings only; none of this stops a run.
    fn core_warnings(
        &self,
        feeder: Option<usize>,
        hot: Option<usize>,
        sqpoll: Option<usize>,
    ) -> Vec<String> {
        let threads: Vec<(&str, usize)> = [("feeder", feeder), ("hot", hot)]
            .into_iter()
            .filter_map(|(role, core)| Some((role, core?)))
            .collect();
        // The polling thread's core is judged by the same three lists: it
        // spins and picks up submissions, so another task, the tick or RCU
        // callbacks there delay them as they delay the hot thread.
        let pinned: Vec<(&str, usize)> = threads
            .iter()
            .copied()
            .chain(sqpoll.map(|core| ("sqpoll", core)))
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
            Some(CpuList::Unreadable | CpuList::Refused) | None => false,
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
                    // A refused nohz_full sets no CPU, so it offloads none.
                    Some(CpuList::Unreadable | CpuList::Refused) | None => true,
                })
                .collect();
            // A refused rcu_nocbs is reported whatever nohz_full offloads:
            // the operator wrote a list and got every CPU instead.
            let checked = if matches!(self.cpu_list("rcu_nocbs"), Some(CpuList::Refused)) {
                &pinned
            } else {
                &not_offloaded
            };
            warnings.extend(self.list_warnings(
                "rcu_nocbs",
                "offloaded from RCU callbacks",
                checked,
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
        if let Some(sqpoll) = sqpoll {
            warnings.extend(self.sqpoll_warnings(&threads, sqpoll));
        }
        warnings
    }

    /// What a reader must weigh about the core given to the `compio-sqpoll`
    /// arm's kernel polling thread, beside the three lists: the thread spins at
    /// close to 100%, so it takes about half of whatever shares its physical
    /// core, and it works on the hot thread's ring, so another NUMA node puts
    /// that memory a node away from one of the two.
    fn sqpoll_warnings(&self, pinned: &[(&str, usize)], sqpoll: usize) -> Vec<String> {
        let mut warnings = Vec::new();
        let prefix = format!("cpu{sqpoll} thread siblings: ");
        let siblings = self
            .section("smt")
            .and_then(|body| body.lines().find_map(|l| l.strip_prefix(prefix.as_str())));
        for &(role, core) in pinned {
            if core == sqpoll {
                let against = if role == "hot" {
                    "the hot thread"
                } else {
                    "the feeder and sink"
                };
                warnings.push(format!(
                    "sqpoll core {sqpoll} is the {role} core: the kernel polling thread spins there against {against}"
                ));
            } else if let Some(siblings) = siblings {
                if sysfs_list_contains(siblings, core) {
                    warnings.push(format!(
                        "sqpoll core {sqpoll} shares its physical core with {role} core {core} (siblings {siblings})"
                    ));
                }
            }
        }
        let hot = pinned.iter().find(|(role, _)| *role == "hot");
        if let Some(&(_, hot)) = hot {
            if let (Some(ours), Some(theirs)) = (self.numa_node(sqpoll), self.numa_node(hot)) {
                if ours != theirs {
                    warnings.push(format!(
                        "sqpoll core {sqpoll} is on NUMA node {ours} and hot core {hot} is on NUMA node {theirs}"
                    ));
                }
            }
        }
        warnings
    }

    /// The NUMA node whose `nodeN cpus:` line lists `cpu`, if the capture
    /// has one.
    fn numa_node(&self, cpu: usize) -> Option<&str> {
        self.section("numa")?.lines().find_map(|line| {
            let (node, cpus) = line.strip_prefix("node")?.split_once(" cpus: ")?;
            sysfs_list_contains(cpus, cpu).then_some(node)
        })
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

/// The flags `isolcpus=` takes before its CPU list, each with the comma the
/// kernel matches it with (Linux v6.8 `kernel/sched/isolation.c`,
/// `housekeeping_isolcpus_setup`: `strncmp(str, "domain,", 7)`).
const ISOLCPUS_FLAGS: [&str; 3] = ["nohz,", "domain,", "managed_irq,"];

/// The CPU list of an `isolcpus=` value, after its flags, as
/// `housekeeping_isolcpus_setup` finds it (v6.8 `kernel/sched/isolation.c`).
/// While the value starts with a letter the kernel takes a word:
///
/// - one of the three flags with its comma is taken;
/// - any other word of letters and underscores is skipped up to its comma
///   ("isolcpus: Skipped unknown flag"), so `all` and `N` are such words too;
/// - a word with any other character is "isolcpus: Invalid flag": the
///   function returns 0 and nothing is isolated ([`CpuList::Refused`]);
/// - a last word with no comma after it, even a known flag, is skipped up to
///   the terminator and the loop's `str++` then steps past it. What the
///   kernel reads next is not settled by the value, so the harness does not
///   read it ([`CpuList::Unreadable`]).
///
/// Whatever follows the flags goes to `cpulist_parse` whole.
fn isolcpus_list(value: &str) -> Result<&str, CpuList> {
    let mut rest = value;
    while let Some(first) = rest.chars().next() {
        if !first.is_ascii() {
            // The kernel's isalpha is not ASCII-only.
            return Err(CpuList::Unreadable);
        }
        if !first.is_ascii_alphabetic() {
            break;
        }
        if let Some(after) = ISOLCPUS_FLAGS.iter().find_map(|f| rest.strip_prefix(f)) {
            rest = after;
            continue;
        }
        let (word, after) = match rest.split_once(',') {
            Some((word, after)) => (word, Some(after)),
            None => (rest, None),
        };
        if !word.chars().all(|c| c.is_ascii_alphabetic() || c == '_') {
            return Err(if word.is_ascii() {
                CpuList::Refused
            } else {
                CpuList::Unreadable
            });
        }
        rest = after.ok_or(CpuList::Unreadable)?;
    }
    Ok(rest)
}

/// The CPUs a kernel command-line CPU list names.
enum CpuList {
    /// The word `all`.
    All,
    /// The CPUs in any of these ranges.
    Listed(Vec<CpuRange>),
    /// Set, with a part only the host can settle, which this harness does
    /// not read.
    Unreadable,
    /// Set, and refused by the kernel on any host.
    Refused,
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
    fn contains(&self, cpu: usize) -> bool {
        (self.first..=self.last).contains(&cpu) && (cpu - self.first) % self.group < self.used
    }
}

/// A number in a CPU list: digits, or the kernel's `N` for the last CPU.
#[derive(Clone, Copy)]
enum Num {
    Is(usize),
    LastCpu,
}

/// Reads one number as `bitmap_getnum` does (v6.8 `lib/bitmap-str.c`): `N`,
/// or decimal digits with no sign that fit an `unsigned int`. `None` is the
/// kernel's -EINVAL or -EOVERFLOW.
fn getnum(s: &str) -> Option<(Num, &str)> {
    if let Some(rest) = s.strip_prefix('N') {
        return Some((Num::LastCpu, rest));
    }
    let len = s.bytes().take_while(u8::is_ascii_digit).count();
    let (digits, rest) = s.split_at(len);
    let n: u32 = digits.parse().ok()?;
    Some((Num::Is(usize::try_from(n).ok()?), rest))
}

/// How one non-empty part of a CPU list reads.
enum Region {
    /// The CPUs of a range.
    Range(CpuRange),
    /// The word `all`.
    All,
    /// Accepted or refused depending on the host's CPU count: it uses `N`,
    /// or `all` with a stride.
    HostDependent,
    /// Refused by the kernel whatever the host.
    Refused,
}

impl Region {
    /// Reads a part the way `bitmap_parse_region` and `bitmap_check_region`
    /// do (v6.8 `lib/bitmap-str.c`). Refused on any host: a part that starts
    /// with no digit, `N` or `all`; anything but `-`, `:` or the end after
    /// the first number; a stride without its `/`; a number that does not
    /// fit 32 bits; and, once the numbers are known, a descending range, a
    /// group of 0, `used > group` (-EINVAL) or a CPU at or past [`MAX_CPUS`]
    /// (-ERANGE).
    fn parse(part: &str) -> Self {
        use Num::{Is, LastCpu};
        let (first, last, rest) = if part
            .get(..3)
            .is_some_and(|word| word.eq_ignore_ascii_case("all"))
        {
            (Is(0), LastCpu, &part[3..])
        } else {
            let Some((first, rest)) = getnum(part) else {
                return Self::Refused;
            };
            match rest.strip_prefix('-') {
                None if rest.is_empty() => (first, first, rest),
                None => return Self::Refused,
                Some(rest) => match getnum(rest) {
                    Some((last, rest)) => (first, last, rest),
                    None => return Self::Refused,
                },
            }
        };
        let (stride, rest) = if rest.is_empty() {
            (None, rest)
        } else {
            let stride = rest.strip_prefix(':').and_then(|rest| {
                let (used, rest) = getnum(rest)?;
                let (group, rest) = getnum(rest.strip_prefix('/')?)?;
                Some(((used, group), rest))
            });
            match stride {
                Some((stride, rest)) => (Some(stride), rest),
                None => return Self::Refused,
            }
        };
        let own = Self::classify(first, last, stride);
        if rest.is_empty() {
            return own;
        }
        // What follows a stride starts the next region, with no comma
        // needed. `bitmap_parselist` checks this region before it parses the
        // next, so a refusal here stands whatever follows.
        match (own, Self::parse(rest)) {
            (Self::Refused, _) | (_, Self::Refused) => Self::Refused,
            _ => Self::HostDependent,
        }
    }

    /// What `bitmap_check_region` makes of one region's numbers.
    fn classify(first: Num, last: Num, stride: Option<(Num, Num)>) -> Self {
        use Num::{Is, LastCpu};
        match stride {
            Some((_, Is(0))) => return Self::Refused,
            Some((Is(used), Is(group))) if used > group => return Self::Refused,
            _ => {}
        }
        // A plain range is one group, all of it used.
        match (first, last, stride.unwrap_or((Is(1), Is(1)))) {
            (Is(0), LastCpu, _) if stride.is_none() => Self::All,
            (Is(first), Is(last), (Is(used), Is(group))) => {
                if first <= last && last < MAX_CPUS {
                    Self::Range(CpuRange {
                        first,
                        last,
                        used,
                        group,
                    })
                } else {
                    Self::Refused
                }
            }
            (Is(first), Is(last), _) if first > last || last >= MAX_CPUS => Self::Refused,
            _ => Self::HostDependent,
        }
    }
}

/// "feeder core 2 and hot core 3" (or "a, b and c") with the verb that
/// agrees with it.
fn name_cores(pinned: &[(&str, usize)]) -> (String, &'static str) {
    let cores: Vec<String> = pinned
        .iter()
        .map(|(role, core)| format!("{role} core {core}"))
        .collect();
    let verb = if cores.len() == 1 { "is" } else { "are" };
    let named = match cores.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => cores.concat(),
    };
    (named, verb)
}

/// Whether a CPU list as sysfs prints one (`cpulist`,
/// `thread_siblings_list`: `0-3,8` from `%*pbl`) names `cpu`. A part that is
/// not a number or an ascending pair of numbers names nothing.
fn sysfs_list_contains(list: &str, cpu: usize) -> bool {
    list.trim().split(',').any(|part| {
        let (from, to) = part.split_once('-').unwrap_or((part, part));
        match (from.parse::<usize>(), to.parse::<usize>()) {
            (Ok(from), Ok(to)) => (from..=to).contains(&cpu),
            _ => false,
        }
    })
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
        Self::captured_with_sqpoll(path, facts, feeder_core, hot_core, None)
    }

    /// [`Self::captured`], with warnings too about the core the
    /// `compio-sqpoll` arm's kernel polling thread was given (`--sqpoll-core`).
    #[must_use]
    pub fn captured_with_sqpoll(
        path: &Path,
        facts: &HostFacts,
        feeder_core: Option<usize>,
        hot_core: Option<usize>,
        sqpoll_core: Option<usize>,
    ) -> Self {
        Self::Captured {
            path: path.display().to_string(),
            captured_utc: facts.captured_utc().to_owned(),
            unavailable: facts.unavailable().into_iter().map(str::to_owned).collect(),
            manual_rtt: facts.has_manual_rtt(),
            warnings: facts.core_warnings(feeder_core, hot_core, sqpoll_core),
        }
    }
}
