//! The host-facts file: what `scripts/capture-host.sh` writes, what the
//! harness requires of it, and what the report says about it.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::process::Command;
use std::time::Duration;

use hl_replay::{render_report, HostFacts, HostFactsNote, HostInfo, ReplayError, RunMeta};

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/capture-host.sh")
}
#[cfg(target_os = "linux")]
fn bash() -> &'static str {
    ["/bin/bash", "/usr/bin/bash"]
        .into_iter()
        .find(|p| Path::new(p).exists())
        .expect("bash")
}
fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hl-replay-facts-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A well-formed file with every required section, each body `body`.
fn file_with(skip: Option<&str>, body: &str) -> String {
    let mut s = String::from(
        "hl-host-facts v1\ncaptured-utc: 2026-11-24T02:00:00Z\nkernel: 6.8.0-test\niface: eth0\n",
    );
    for name in HostFacts::REQUIRED {
        if Some(name) == skip {
            continue;
        }
        let _ = writeln!(s, "== begin {name}\n{body}\n== end {name}");
    }
    s
}
fn refused(text: &str) -> String {
    match HostFacts::parse(text) {
        Err(ReplayError::HostFacts(why)) => why,
        other => panic!("expected a host-facts refusal, got {other:?}"),
    }
}

#[test]
fn a_complete_file_parses_and_exposes_its_sections() {
    let f = HostFacts::parse(&file_with(None, "some fact\nanother")).unwrap();
    assert_eq!(f.captured_utc(), "2026-11-24T02:00:00Z");
    assert_eq!(f.kernel(), "6.8.0-test");
    for name in HostFacts::REQUIRED {
        assert_eq!(f.section(name), Some("some fact\nanother"), "{name}");
    }
    assert_eq!(f.unavailable(), Vec::<&str>::new());
    assert!(!f.has_manual_rtt());
}

#[test]
fn the_required_sections_are_the_agreed_list() {
    assert_eq!(
        HostFacts::REQUIRED,
        [
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
        ]
    );
}

#[test]
fn every_missing_section_is_refused_by_name() {
    for name in HostFacts::REQUIRED {
        let why = refused(&file_with(Some(name), "x"));
        assert!(why.contains(name), "{name}: {why}");
    }
}

#[test]
fn an_empty_or_unterminated_section_is_refused() {
    let why = refused(&file_with(None, ""));
    assert!(why.contains("empty"), "{why}");
    let cut = file_with(None, "x").replace("== end chrony\n", "");
    let why = refused(&cut);
    assert!(why.contains("chrony"), "{why}");
}

#[test]
fn a_file_without_the_header_is_refused() {
    let no_magic = file_with(None, "x").replace("hl-host-facts v1\n", "");
    assert!(refused(&no_magic).contains("hl-host-facts v1"));
    let no_kernel = file_with(None, "x").replace("kernel: 6.8.0-test\n", "");
    assert!(refused(&no_kernel).contains("kernel"));
    let no_time = file_with(None, "x").replace("captured-utc: 2026-11-24T02:00:00Z\n", "");
    assert!(refused(&no_time).contains("captured-utc"));
    assert!(refused("").contains("hl-host-facts v1"));
}

#[test]
fn unavailable_is_a_fact_not_a_gap() {
    let text = file_with(None, "x")
        .replace(
            "== begin chrony\nx\n",
            "== begin chrony\nunavailable (chronyc: not installed)\n",
        )
        .replace(
            "== begin thp\nx\n",
            "== begin thp\nenabled: always\ndefrag: unavailable (not readable)\n",
        );
    let f = HostFacts::parse(&text).unwrap();
    assert_eq!(f.unavailable(), ["thp", "chrony"]);
}

#[test]
fn the_manual_rtt_section_is_optional_and_noticed() {
    let mut text = file_with(None, "x");
    assert!(!HostFacts::parse(&text).unwrap().has_manual_rtt());
    text.push_str("== begin rtt-manual\nrtt min/avg/max = 1/2/3 ms, n=100\n== end rtt-manual\n");
    let f = HostFacts::parse(&text).unwrap();
    assert!(f.has_manual_rtt());
    assert_eq!(HostFacts::MANUAL_RTT, "rtt-manual");
    assert!(!HostFacts::REQUIRED.contains(&HostFacts::MANUAL_RTT));
}

#[test]
fn facts_from_another_kernel_are_refused() {
    let f = HostFacts::parse(&file_with(None, "x")).unwrap();
    f.check_kernel("6.8.0-test").unwrap();
    match f.check_kernel("5.15.0-other") {
        Err(ReplayError::HostFacts(why)) => {
            assert!(
                why.contains("6.8.0-test") && why.contains("5.15.0-other"),
                "{why}"
            );
        }
        other => panic!("{other:?}"),
    }
}

#[cfg(target_os = "linux")]
fn capture(dir: &str, path_env: Option<&str>) -> String {
    let out = tmp(dir).join("host-facts.txt");
    let mut c = Command::new(bash());
    c.arg(script()).arg(&out);
    if let Some(p) = path_env {
        c.env("PATH", p);
    }
    let o = c.output().unwrap();
    assert!(
        o.status.success(),
        "capture-host.sh ended with {:?}; stderr: {:?}",
        o.status,
        String::from_utf8_lossy(&o.stderr)
    );
    std::fs::read_to_string(out).unwrap()
}

// The capture script is for Linux hosts (/proc, /sys, bash 4.2+).
#[cfg(target_os = "linux")]
#[test]
fn the_capture_script_writes_a_file_the_harness_accepts() {
    let text = capture("real", None);
    let f = HostFacts::parse(&text).unwrap();
    f.check_kernel(&HostInfo::detect().kernel).unwrap();
    assert!(!f.has_manual_rtt(), "the script must never write the RTT");
}

// The capture script is for Linux hosts (/proc, /sys, bash 4.2+).
#[cfg(target_os = "linux")]
#[test]
fn the_capture_script_writes_the_facts_latency_asked_for() {
    let f = HostFacts::parse(&capture("wide", None)).unwrap();
    let has = |section: &str, wants: &[&str]| {
        let body = f.section(section).unwrap();
        for want in wants {
            assert!(
                body.lines().any(|l| l.starts_with(want)),
                "{section}: no line starting {want:?} in:\n{body}"
            );
        }
    };
    has(
        "kernel-cmdline",
        &[
            "isolcpus: ",
            "nohz_full: ",
            "rcu_nocbs: ",
            "intel_idle.max_cstate: ",
            "processor.max_cstate: ",
        ],
    );
    has(
        "busy-poll-sysctls",
        &[
            "net.core.busy_poll: ",
            "net.core.busy_read: ",
            "net.core.netdev_budget: ",
            "net.core.netdev_budget_usecs: ",
            "kernel.sched_rt_runtime_us: ",
            "kernel.numa_balancing: ",
            "kernel.timer_migration: ",
            "vm.stat_interval: ",
        ],
    );
    // Every online CPU, not only cpu0.
    has("cstates", &["cpu0 ", "cpu1 ", "cpu_dma_latency: "]);
    has(
        "hugepages-mlock",
        &["HugePages_Total: ", "max locked memory (ulimit -l): "],
    );
    has(
        "smt",
        &[
            "active: ",
            "cpu0 thread siblings: ",
            "cpu1 thread siblings: ",
        ],
    );
}

/// Command-line lines under which no core is outside `nohz_full` or `rcu_nocbs`.
const QUIET: &str = "nohz_full: all\nrcu_nocbs: all";

/// A complete file whose kernel-cmdline and smt sections hold `cmdline` and `smt`.
fn file_with_cores(cmdline: &str, smt: &str) -> HostFacts {
    let text = file_with(None, "x")
        .replace(
            "== begin kernel-cmdline\nx\n",
            &format!("== begin kernel-cmdline\n{cmdline}\n"),
        )
        .replace("== begin smt\nx\n", &format!("== begin smt\n{smt}\n"));
    HostFacts::parse(&text).unwrap()
}
fn warnings(f: &HostFacts, feeder: Option<usize>, hot: Option<usize>) -> Vec<String> {
    match HostFactsNote::captured(Path::new("f"), f, feeder, hot) {
        HostFactsNote::Captured { warnings, .. } => warnings,
        other => panic!("{other:?}"),
    }
}

#[test]
fn isolcpus_is_parsed_with_flags_ranges_and_singles() {
    let f = file_with_cores(
        "quiet isolcpus=domain,managed_irq,2-3,6\nisolcpus: domain,managed_irq,2-3,6\nnohz_full: 2-3",
        "active: 0",
    );
    assert_eq!(f.isolated_cpus(), Some(vec![2, 3, 6]));
    let f = file_with_cores("quiet\nisolcpus: not set", "active: 0");
    assert_eq!(f.isolated_cpus(), None);
    let f = file_with_cores("quiet", "active: 0");
    assert_eq!(f.isolated_cpus(), None);
}

#[test]
fn cores_outside_the_isolated_set_are_a_warning_not_a_failure() {
    let smt = "active: 0\ncpu2 thread siblings: 2\ncpu3 thread siblings: 3";
    let f = file_with_cores(&format!("isolcpus: 2-3\n{QUIET}"), smt);
    assert_eq!(warnings(&f, Some(2), Some(3)), Vec::<String>::new());
    assert_eq!(
        warnings(&f, Some(1), Some(3)),
        ["feeder core 1 is not in isolcpus (2-3)"]
    );
    assert_eq!(
        warnings(&f, Some(2), Some(1)),
        ["hot core 1 is not in isolcpus (2-3)"]
    );
    // Unpinned threads have no core to check.
    assert_eq!(warnings(&f, None, None), Vec::<String>::new());
    let f = file_with_cores(&format!("isolcpus: not set\n{QUIET}"), smt);
    assert_eq!(
        warnings(&f, Some(2), Some(3)),
        ["isolcpus is not set: feeder core 2 and hot core 3 are not isolated"]
    );
}

#[test]
fn a_sibling_thread_on_the_hot_core_is_a_warning() {
    let f = file_with_cores(
        &format!("isolcpus: 2-3\n{QUIET}"),
        "active: 1\ncpu2 thread siblings: 2,50\ncpu3 thread siblings: 3,51",
    );
    assert_eq!(
        warnings(&f, Some(2), Some(3)),
        ["hot core 3 shares its physical core with another thread (siblings 3,51)"]
    );
}

#[test]
fn the_report_prints_host_fact_warnings() {
    let f = file_with_cores(
        "isolcpus: 2-3\nnohz_full: 2-3\nrcu_nocbs: 2-3",
        "active: 0\ncpu1 thread siblings: 1",
    );
    let note = HostFactsNote::captured(Path::new("f"), &f, Some(2), Some(1));
    let report = render_report(&meta(note), &[]);
    for want in [
        "  warning: hot core 1 is not in isolcpus (2-3)",
        "  warning: hot core 1 is not in nohz_full (2-3)",
        "  warning: hot core 1 is not in rcu_nocbs (2-3)",
    ] {
        assert!(report.contains(want), "no {want:?} in:\n{report}");
    }
}

/// The warnings for a capture whose parsed command-line lines are `lines`.
fn list_warnings(lines: &str, feeder: Option<usize>, hot: Option<usize>) -> Vec<String> {
    let smt = "active: 0\ncpu2 thread siblings: 2\ncpu3 thread siblings: 3";
    warnings(&file_with_cores(lines, smt), feeder, hot)
}

#[test]
fn cores_outside_nohz_full_warn_as_isolcpus_does() {
    let rest = "isolcpus: 0-7\nrcu_nocbs: all";
    let none = Vec::<String>::new();
    assert_eq!(
        list_warnings(&format!("{rest}\nnohz_full: 2-3"), Some(2), Some(3)),
        none
    );
    assert_eq!(
        list_warnings(&format!("{rest}\nnohz_full: 2-3"), Some(0), Some(3)),
        ["feeder core 0 is not in nohz_full (2-3)"]
    );
    assert_eq!(
        list_warnings(&format!("{rest}\nnohz_full: 2-3"), Some(2), Some(1)),
        ["hot core 1 is not in nohz_full (2-3)"]
    );
    assert_eq!(
        list_warnings(&format!("{rest}\nnohz_full: not set"), Some(2), Some(3)),
        ["nohz_full is not set: feeder core 2 and hot core 3 are not tickless"]
    );
    assert_eq!(
        list_warnings(&format!("{rest}\nnohz_full: not set"), None, Some(3)),
        ["nohz_full is not set: hot core 3 is not tickless"]
    );
    // A capture from before the script wrote the line reads as not set.
    assert_eq!(
        list_warnings(rest, None, Some(3)),
        ["nohz_full is not set: hot core 3 is not tickless"]
    );
    // Unpinned threads have no core to check.
    assert_eq!(
        list_warnings(&format!("{rest}\nnohz_full: not set"), None, None),
        none
    );
}

#[test]
fn cores_outside_rcu_nocbs_warn_as_isolcpus_does() {
    // nohz_full names neither pinned core, so rcu_nocbs alone decides; the
    // nohz_full warnings that come with that are left out here.
    let rest = "isolcpus: 0-7\nnohz_full: 6-7";
    let rcu = |nocbs: &str, feeder, hot| -> Vec<String> {
        list_warnings(&format!("{rest}\nrcu_nocbs: {nocbs}"), feeder, hot)
            .into_iter()
            .filter(|w| w.contains("rcu_nocbs"))
            .collect()
    };
    let none = Vec::<String>::new();
    assert_eq!(rcu("2-3", Some(2), Some(3)), none);
    assert_eq!(
        rcu("2-3", Some(0), Some(3)),
        ["feeder core 0 is not in rcu_nocbs (2-3)"]
    );
    assert_eq!(
        rcu("2-3", Some(2), Some(1)),
        ["hot core 1 is not in rcu_nocbs (2-3)"]
    );
    assert_eq!(
        rcu("not set", Some(2), Some(3)),
        ["rcu_nocbs is not set: feeder core 2 and hot core 3 are not offloaded from RCU callbacks"]
    );
    assert_eq!(rcu("all", Some(2), Some(3)), none);
    // The order in the report: isolcpus, then nohz_full, then rcu_nocbs.
    assert_eq!(
        list_warnings(&format!("{rest}\nrcu_nocbs: 2-3"), Some(0), Some(3)),
        [
            "feeder core 0 is not in nohz_full (6-7)",
            "hot core 3 is not in nohz_full (6-7)",
            "feeder core 0 is not in rcu_nocbs (2-3)"
        ]
    );
    // `all` and a stepped range (CPUs 0, 1, 4, 5) are CPU lists too.
    let stepped = "isolcpus: 0-7\nnohz_full: 0-7:2/4\nrcu_nocbs: all";
    assert_eq!(list_warnings(stepped, Some(1), Some(5)), none);
    assert_eq!(
        list_warnings(stepped, Some(2), Some(5)),
        ["feeder core 2 is not in nohz_full (0-7:2/4)"]
    );
}

#[test]
fn a_nohz_full_core_has_its_rcu_callbacks_offloaded_already() {
    // The kernel offloads RCU callbacks on every nohz_full CPU, so a core in
    // nohz_full is not reported as missing from rcu_nocbs.
    let none = Vec::<String>::new();
    assert_eq!(
        list_warnings(
            "isolcpus: 2-3\nnohz_full: 2-3\nrcu_nocbs: not set",
            Some(2),
            Some(3)
        ),
        none
    );
    assert_eq!(
        list_warnings(
            "isolcpus: 2-3\nnohz_full: 2\nrcu_nocbs: not set",
            Some(2),
            Some(3)
        ),
        [
            "hot core 3 is not in nohz_full (2)",
            "rcu_nocbs is not set: hot core 3 is not offloaded from RCU callbacks"
        ]
    );
}

#[test]
fn a_list_the_harness_cannot_read_is_not_called_unset() {
    // `N` (the last CPU) is valid to the kernel and not resolved here.
    assert_eq!(
        list_warnings("isolcpus: 2-3\nnohz_full: 2-N\nrcu_nocbs: ALL", Some(2), Some(3)),
        ["nohz_full (2-N) is not a CPU list this harness can read: pinned cores not checked against it"]
    );
}

#[test]
fn a_list_with_one_unreadable_part_is_unreadable_as_a_whole() {
    // `0,2-N` names CPU 0 and CPUs 2 to the last: reading it as [0] would
    // report core 3 as outside a list that holds it.
    let unread = |key: &str, raw: &str| {
        format!("{key} ({raw}) is not a CPU list this harness can read: pinned cores not checked against it")
    };
    assert_eq!(
        list_warnings(
            "isolcpus: 0-7\nnohz_full: 0,2-N\nrcu_nocbs: all",
            Some(2),
            Some(3)
        ),
        [unread("nohz_full", "0,2-N")]
    );
    assert_eq!(
        list_warnings(
            "isolcpus: 0-7\nnohz_full: all\nrcu_nocbs: 2,x",
            Some(2),
            Some(3)
        ),
        [rcu_refused("2,x")],
        "the operator is told the list was replaced, whatever nohz_full offloads"
    );
    assert_eq!(
        list_warnings(
            "isolcpus: 0-7\nnohz_full: 6-7\nrcu_nocbs: 2,x",
            Some(2),
            None
        ),
        [
            "feeder core 2 is not in nohz_full (6-7)".to_owned(),
            rcu_refused("2,x")
        ]
    );
    // isolcpus takes the kernel's three flags before its list and no other word.
    assert_eq!(
        list_warnings(
            &format!("isolcpus: nohz,domain,managed_irq,2-3\n{QUIET}"),
            Some(2),
            Some(3)
        ),
        Vec::<String>::new()
    );
    assert_eq!(
        list_warnings(&format!("isolcpus: 2,3-N\n{QUIET}"), Some(2), Some(3)),
        [unread("isolcpus", "2,3-N")]
    );
    // The flags belong to isolcpus alone.
    assert_eq!(
        list_warnings(
            "isolcpus: 2-3\nnohz_full: domain,2-3\nrcu_nocbs: all",
            Some(2),
            Some(3)
        ),
        [ignored(
            "nohz_full",
            "domain,2-3",
            "feeder core 2 and hot core 3 are not tickless"
        )]
    );
    let f = file_with_cores("isolcpus: 2,3-N", "active: 0");
    assert_eq!(
        f.isolated_cpus(),
        None,
        "a partly read list is not returned"
    );
}

#[test]
fn an_unreadable_nohz_full_leaves_rcu_offload_undetermined() {
    // nohz_full CPUs are offloaded whatever rcu_nocbs says, so with nohz_full
    // unread a core outside rcu_nocbs may or may not be offloaded.
    let unread =
        "nohz_full (2-N) is not a CPU list this harness can read: pinned cores not checked against it";
    assert_eq!(
        list_warnings("isolcpus: 2-3\nnohz_full: 2-N\nrcu_nocbs: not set", Some(2), Some(3)),
        [
            unread,
            "rcu_nocbs is not set and nohz_full (2-N) could not be parsed: whether feeder core 2 and hot core 3 are offloaded from RCU callbacks is undetermined"
        ]
    );
    assert_eq!(
        list_warnings("isolcpus: 2-3\nnohz_full: 2-N\nrcu_nocbs: not set", None, Some(3)),
        [
            unread,
            "rcu_nocbs is not set and nohz_full (2-N) could not be parsed: whether hot core 3 is offloaded from RCU callbacks is undetermined"
        ]
    );
    assert_eq!(
        list_warnings("isolcpus: 2-3\nnohz_full: 2-N\nrcu_nocbs: 2", Some(2), Some(3)),
        [
            unread,
            "hot core 3 is not in rcu_nocbs (2) and nohz_full (2-N) could not be parsed: whether it is offloaded from RCU callbacks is undetermined"
        ]
    );
    // A core rcu_nocbs names is offloaded whatever nohz_full says.
    assert_eq!(
        list_warnings(
            "isolcpus: 2-3\nnohz_full: 2-N\nrcu_nocbs: 2-3",
            Some(2),
            Some(3)
        ),
        [unread]
    );
}

#[test]
fn a_pinned_cpu_0_is_weighed_as_the_boot_cpu() {
    // Linux v6.8 Documentation/admin-guide/kernel-parameters.txt, nohz_full=:
    // "The boot CPU will be forced outside the range to maintain the
    // timekeeping." So CPU 0 outside nohz_full is truly not tickless and the
    // warning stays; CPU 0 named in nohz_full may not be tickless either.
    let rest = "isolcpus: 0-7\nrcu_nocbs: all";
    assert_eq!(
        list_warnings(&format!("{rest}\nnohz_full: 1-7"), Some(2), Some(0)),
        ["hot core 0 is not in nohz_full (1-7)"]
    );
    assert_eq!(
        list_warnings(&format!("{rest}\nnohz_full: 0-7"), Some(0), Some(3)),
        ["feeder core 0 is in nohz_full (0-7), but the kernel forces the boot CPU (normally CPU 0) outside nohz_full: it may still take the tick"]
    );
    assert_eq!(
        list_warnings(&format!("{rest}\nnohz_full: all"), Some(2), Some(0)),
        ["hot core 0 is in nohz_full (all), but the kernel forces the boot CPU (normally CPU 0) outside nohz_full: it may still take the tick"]
    );
}

#[test]
fn a_run_with_every_list_unset_still_gets_a_report() {
    // Warnings never stop a run: the note is built and the report renders.
    let f = file_with_cores(
        "isolcpus: not set\nnohz_full: not set\nrcu_nocbs: not set",
        "active: 0",
    );
    let note = HostFactsNote::captured(Path::new("f"), &f, Some(2), Some(3));
    let report = render_report(&meta(note), &[]);
    assert_eq!(report.matches("  warning: ").count(), 3, "{report}");
}

#[cfg(target_os = "linux")]
#[test]
fn with_no_tools_at_all_every_section_is_still_written() {
    // No `lscpu`, `ethtool`, `chronyc` or anything else on PATH: what cannot
    // be read says so and nothing is left out.
    let text = capture("bare", Some("/nonexistent-hl-replay-path"));
    let f = HostFacts::parse(&text).unwrap();
    for name in ["lscpu", "ethtool-c", "ethtool-k", "ethtool-l", "chrony"] {
        let body = f.section(name).unwrap();
        assert!(body.starts_with("unavailable"), "{name}: {body}");
        assert!(f.unavailable().contains(&name), "{name}");
    }
}

#[test]
fn the_capture_script_names_no_exchange_host_and_makes_no_network_probe() {
    let src = std::fs::read_to_string(script()).unwrap().to_lowercase();
    for banned in [
        "hyperliquid",
        "curl",
        "wget",
        "ping",
        "traceroute",
        "mtr ",
        "/dev/tcp",
        "/dev/udp",
        "ssh ",
        "nc ",
    ] {
        assert!(!src.contains(banned), "capture-host.sh contains {banned:?}");
    }
    // Read-only: it sets nothing.
    for banned in [
        "sysctl -w",
        "ethtool -g ",
        "ethtool -s ",
        "sudo",
        "tee /",
        "> /proc",
        "> /sys",
    ] {
        assert!(!src.contains(banned), "capture-host.sh contains {banned:?}");
    }
}

fn meta(host_facts: HostFactsNote) -> RunMeta {
    RunMeta {
        commit: "0".repeat(40),
        host: HostInfo::default(),
        feeder_core: None,
        hot_core: None,
        rounds: 1,
        measured_per_round: 1,
        warmup_per_round: 0,
        interval: Duration::from_micros(1),
        input: "none".to_owned(),
        host_facts,
    }
}

#[test]
fn the_report_says_which_host_facts_a_run_carries() {
    let text = file_with(None, "x").replace(
        "== begin chrony\nx\n",
        "== begin chrony\nunavailable (chronyc: not installed)\n",
    );
    let f = HostFacts::parse(&text).unwrap();
    let note = HostFactsNote::captured(Path::new("/tmp/facts.txt"), &f, None, None);
    assert_eq!(
        note,
        HostFactsNote::Captured {
            path: "/tmp/facts.txt".to_owned(),
            captured_utc: "2026-11-24T02:00:00Z".to_owned(),
            unavailable: vec!["chrony".to_owned()],
            manual_rtt: false,
            warnings: Vec::new(),
        }
    );
    let report = render_report(&meta(note), &[]);
    assert!(
        report.contains(
            "host facts: /tmp/facts.txt, captured 2026-11-24T02:00:00Z, 18 sections checked, unavailable in: chrony"
        ),
        "{report}"
    );
    assert!(
        report.contains("rtt-manual: absent (operator step, never checked by the harness)"),
        "{report}"
    );
}

#[test]
fn the_report_says_when_host_facts_were_waived() {
    let report = render_report(&meta(HostFactsNote::Waived), &[]);
    assert!(
        report.contains(
            "host facts: NOT CAPTURED (--allow-missing-host-facts): local run, not a result for a write-up"
        ),
        "{report}"
    );
    let report = render_report(&meta(HostFactsNote::NotRequired), &[]);
    assert!(
        report.contains("host facts: not captured (nothing written to disk)"),
        "{report}"
    );
}

#[test]
fn the_checked_bytes_are_what_gets_written_beside_the_report() {
    let text = file_with(None, "x");
    let f = HostFacts::parse(&text).unwrap();
    assert_eq!(f.raw(), text);
    let dir = tmp("write");
    let path = f.write_to(&dir).unwrap();
    assert_eq!(path, dir.join("host-facts.txt"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    // Writing over an existing copy replaces it whole and leaves nothing else.
    let again = f.write_to(&dir).unwrap();
    assert_eq!(std::fs::read_to_string(again).unwrap(), text);
    let names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["host-facts.txt"]);
}

#[test]
fn a_cpu_number_no_kernel_can_hold_is_a_list_the_kernel_refuses() {
    // The kernel refuses a list naming a CPU at or past its mask width
    // (-ERANGE, Linux v6.8 lib/bitmap-str.c bitmap_check_region), and no
    // x86-64 or arm64 kernel is built wider than 8192 CPUs (v6.8
    // arch/x86/Kconfig NR_CPUS_RANGE_END; arm64 stops at 4096). Such a range
    // is never expanded CPU by CPU.
    let none = Vec::<String>::new();
    for raw in [
        "0-18446744073709551615",
        "0-99999999999",
        "2-8192",
        "2-3,8192",
    ] {
        assert_eq!(
            list_warnings(
                &format!("isolcpus: 0-7\nnohz_full: {raw}\nrcu_nocbs: all"),
                Some(2),
                Some(3)
            ),
            [ignored(
                "nohz_full",
                raw,
                "feeder core 2 and hot core 3 are not tickless"
            )],
            "{raw}"
        );
        let f = file_with_cores(&format!("isolcpus: {raw}"), "active: 0");
        assert_eq!(f.isolated_cpus(), Some(Vec::new()), "{raw}");
    }
    // The last CPU number such a kernel can have is still read.
    assert_eq!(
        list_warnings(
            "isolcpus: 0-8191\nnohz_full: 2-8191\nrcu_nocbs: 8191,0-8191:1/2",
            Some(2),
            Some(8191)
        ),
        none
    );
    assert_eq!(
        list_warnings(
            "isolcpus: 0-8191\nnohz_full: 4-8191\nrcu_nocbs: all",
            Some(2),
            Some(8191)
        ),
        ["feeder core 2 is not in nohz_full (4-8191)"]
    );
}

#[test]
fn isolated_cpus_are_listed_once_each_in_ascending_order() {
    // A list may name a CPU many times; the result stays bounded by the CPUs
    // a kernel can have, not by the length of the list.
    let f = file_with_cores("isolcpus: 6,2-3,2-3,0-7:1/8", "active: 0");
    assert_eq!(f.isolated_cpus(), Some(vec![0, 2, 3, 6]));
}

#[test]
fn a_stride_using_more_than_its_group_is_a_list_the_kernel_refuses() {
    // The kernel rejects the whole list when "used size" exceeds "group
    // size" (-EINVAL: `r->off > r->group_len`, Linux v6.8 lib/bitmap-str.c
    // bitmap_check_region), so the kernel ignores nohz_full= and isolcpus=.
    for raw in ["0-7:3/2", "2-3,0-7:5/4"] {
        assert_eq!(
            list_warnings(
                &format!("isolcpus: 0-7\nnohz_full: {raw}\nrcu_nocbs: all"),
                Some(2),
                Some(3)
            ),
            [ignored(
                "nohz_full",
                raw,
                "feeder core 2 and hot core 3 are not tickless"
            )],
            "{raw}"
        );
        let f = file_with_cores(&format!("isolcpus: {raw}"), "active: 0");
        assert_eq!(f.isolated_cpus(), Some(Vec::new()), "{raw}");
    }
    // Used equal to the group is the whole range, which the kernel accepts.
    assert_eq!(
        list_warnings(
            "isolcpus: 0-7\nnohz_full: 0-7:2/2\nrcu_nocbs: all",
            Some(1),
            Some(5)
        ),
        Vec::<String>::new()
    );
}

#[test]
fn a_list_that_selects_no_cpu_is_a_readable_empty_list() {
    // The kernel accepts a stride using 0 of each group and sets no CPU:
    // `bitmap_check_region` refuses only `r->off > r->group_len`, and
    // `bitmap_set_region` then sets `min(r->end - start + 1, r->off)` = 0
    // bits (Linux v6.8 lib/bitmap-str.c). So the list is read, and holds no
    // pinned core.
    for raw in ["0-7:0/2", "0-3:0/1,4-7:0/4"] {
        assert_eq!(
            list_warnings(
                &format!("isolcpus: 0-7\nnohz_full: {raw}\nrcu_nocbs: all"),
                Some(2),
                Some(3)
            ),
            [
                format!("feeder core 2 is not in nohz_full ({raw})"),
                format!("hot core 3 is not in nohz_full ({raw})"),
            ],
            "{raw}"
        );
        let f = file_with_cores(&format!("isolcpus: {raw}"), "active: 0");
        assert_eq!(f.isolated_cpus(), Some(Vec::new()), "{raw}");
    }
    // With nohz_full read as empty, RCU offload is decided by rcu_nocbs
    // alone, never left "undetermined".
    assert_eq!(
        list_warnings(
            "isolcpus: 0-7\nnohz_full: 0-7:0/2\nrcu_nocbs: 3",
            Some(2),
            Some(3)
        ),
        [
            "feeder core 2 is not in nohz_full (0-7:0/2)",
            "hot core 3 is not in nohz_full (0-7:0/2)",
            "feeder core 2 is not in rcu_nocbs (3)",
        ]
    );
    // An empty part beside a part that names CPUs changes nothing.
    assert_eq!(
        list_warnings(
            "isolcpus: 0-7\nnohz_full: 0-7:0/2,2-3\nrcu_nocbs: all",
            Some(2),
            Some(3)
        ),
        Vec::<String>::new()
    );
}

#[test]
fn isolcpus_flags_without_their_last_comma_are_not_read() {
    // `isolcpus=domain,` hands the kernel an empty list, which
    // `bitmap_parselist` accepts as an empty mask. Without the last comma the
    // kernel does not take the last word as a flag (it matches "domain," and
    // "managed_irq,", Linux v6.8 kernel/sched/isolation.c
    // housekeeping_isolcpus_setup): the word falls into the unknown-flag
    // loop, which steps past the end of the value. What the kernel then
    // reads is not settled by the value, so the harness does not read it.
    assert_eq!(
        list_warnings(
            "isolcpus: domain,managed_irq\nnohz_full: 2-3\nrcu_nocbs: all",
            Some(2),
            Some(3)
        ),
        [unread("isolcpus", "domain,managed_irq")]
    );
}

/// The line written when the kernel refuses `rcu_nocbs=`.
fn rcu_refused(raw: &str) -> String {
    format!("rcu_nocbs ({raw}) is a CPU list the kernel refuses: the kernel offloads every CPU from RCU callbacks instead; pinned cores are offloaded")
}

/// The line written when the kernel refuses `nohz_full=` or `isolcpus=`.
fn ignored(key: &str, raw: &str, consequence: &str) -> String {
    format!("{key} ({raw}) is a CPU list the kernel refuses and ignores: {consequence}")
}

/// The line written when a list is set and a part of it is not read.
fn unread(key: &str, raw: &str) -> String {
    format!("{key} ({raw}) is not a CPU list this harness can read: pinned cores not checked against it")
}

#[test]
fn empty_parts_of_a_cpu_list_are_skipped_as_the_kernel_skips_them() {
    // Linux v6.8 lib/bitmap-str.c: bitmap_parselist calls bitmap_find_region
    // before every region, which steps over any run of commas ("The format
    // allows commas and whitespaces at the beginning of the region") and ends
    // the list, with success, at the terminator. So `2,,3`, `,2,3` and `2,3,`
    // all name CPUs 2 and 3.
    let none = Vec::<String>::new();
    for raw in ["2,,3", ",2,3", "2,3,", ",,2,,,3,,", "2-3,", ",2-3"] {
        assert_eq!(
            list_warnings(
                &format!("isolcpus: {raw}\nnohz_full: {raw}\nrcu_nocbs: {raw}"),
                Some(2),
                Some(3)
            ),
            none,
            "{raw}"
        );
        // The cores that are not there are still reported, each list by name.
        assert_eq!(
            list_warnings(
                &format!("isolcpus: {raw}\nnohz_full: {raw}\nrcu_nocbs: {raw}"),
                Some(2),
                Some(4)
            ),
            [
                format!("hot core 4 is not in isolcpus ({raw})"),
                format!("hot core 4 is not in nohz_full ({raw})"),
                format!("hot core 4 is not in rcu_nocbs ({raw})"),
            ],
            "{raw}"
        );
        let f = file_with_cores(&format!("isolcpus: {raw}\n{QUIET}"), "active: 0");
        assert_eq!(f.isolated_cpus(), Some(vec![2, 3]), "{raw}");
    }
    // An empty part does not hide a part the harness cannot read.
    assert_eq!(
        list_warnings(&format!("isolcpus: 2,,3-N\n{QUIET}"), Some(2), Some(3)),
        [unread("isolcpus", "2,,3-N")]
    );
    // `all` beside an empty part is still every CPU.
    assert_eq!(
        list_warnings(
            "isolcpus: 0-7\nnohz_full: all,\nrcu_nocbs: ,all",
            Some(2),
            Some(3)
        ),
        none
    );
}

#[test]
fn a_key_with_nothing_after_it_is_set_and_names_no_cpu() {
    // Linux v6.8: `nohz_full=` and `isolcpus=` hand the empty string to
    // housekeeping_setup (kernel/sched/isolation.c), and `rcu_nocbs=` to
    // rcu_nocb_setup (kernel/rcu/tree_nocb.h). Each calls cpulist_parse,
    // which is bitmap_parselist (include/linux/cpumask.h); that returns 0
    // with no bit set for an empty string (lib/bitmap-str.c). The parameter
    // is accepted and names no CPU: it is not "not set" and not unreadable.
    //
    // The capture script writes such a key as `key: ` and the body of a
    // section loses its trailing whitespace, so the line is tried in the
    // middle of the section, at its end, and with the space already gone.
    for (isolcpus, nohz_full) in [
        ("isolcpus: ", "nohz_full: "),
        ("isolcpus:", "nohz_full:"),
        ("isolcpus: ,", "nohz_full: ,,"),
    ] {
        for lines in [
            format!("{isolcpus}\n{nohz_full}\nrcu_nocbs: all"),
            format!("rcu_nocbs: all\n{isolcpus}\n{nohz_full}"),
            format!("rcu_nocbs: all\n{nohz_full}\n{isolcpus}"),
        ] {
            let shown = |line: &str| line.split_once(':').unwrap().1.trim().to_owned();
            assert_eq!(
                list_warnings(&lines, Some(2), Some(3)),
                [
                    format!("feeder core 2 is not in isolcpus ({})", shown(isolcpus)),
                    format!("hot core 3 is not in isolcpus ({})", shown(isolcpus)),
                    format!("feeder core 2 is not in nohz_full ({})", shown(nohz_full)),
                    format!("hot core 3 is not in nohz_full ({})", shown(nohz_full)),
                ],
                "{lines:?}"
            );
            let f = file_with_cores(&lines, "active: 0");
            assert_eq!(f.isolated_cpus(), Some(Vec::new()), "{lines:?}");
        }
    }
    // An empty nohz_full offloads no core (rcu_init_nohz takes the nohz_full
    // mask only when it is not empty), so rcu_nocbs alone decides.
    assert_eq!(
        list_warnings(
            "isolcpus: 2-3\nnohz_full: \nrcu_nocbs: not set",
            Some(2),
            Some(3)
        ),
        [
            "feeder core 2 is not in nohz_full ()",
            "hot core 3 is not in nohz_full ()",
            "rcu_nocbs is not set: feeder core 2 and hot core 3 are not offloaded from RCU callbacks",
        ]
    );
    // An empty rcu_nocbs is set and offloads no core of its own.
    for rcu_nocbs in ["rcu_nocbs: \nisolcpus: 2-3", "isolcpus: 2-3\nrcu_nocbs:"] {
        assert_eq!(
            list_warnings(&format!("nohz_full: 2\n{rcu_nocbs}"), Some(2), Some(3)),
            [
                "hot core 3 is not in nohz_full (2)",
                "hot core 3 is not in rcu_nocbs ()",
            ],
            "{rcu_nocbs:?}"
        );
    }
    // No core of an empty nohz_full is the boot CPU.
    assert_eq!(
        list_warnings("isolcpus: 0-7\nnohz_full: \nrcu_nocbs: all", Some(0), None),
        ["feeder core 0 is not in nohz_full ()"]
    );
    // A longer key is another key.
    let f = file_with_cores(&format!("isolcpus_x: 2\n{QUIET}"), "active: 0");
    assert_eq!(f.isolated_cpus(), None);
}

#[test]
fn isolcpus_flags_are_read_only_where_the_kernel_reads_them() {
    // Linux v6.8 kernel/sched/isolation.c, housekeeping_isolcpus_setup: the
    // flags are taken while the value starts with a letter, each with its
    // comma; whatever follows goes to cpulist_parse whole. So an empty part
    // ends the flags, and a flag after it or after a CPU is a letter in the
    // list, which bitmap_parse_region refuses (-EINVAL).
    let f = file_with_cores(&format!("isolcpus: domain,,2,\n{QUIET}"), "active: 0");
    assert_eq!(f.isolated_cpus(), Some(vec![2]));
    let f = file_with_cores(&format!("isolcpus: domain,\n{QUIET}"), "active: 0");
    assert_eq!(f.isolated_cpus(), Some(Vec::new()));
    for raw in [",domain,2", "2,domain", "domain,,managed_irq,2"] {
        assert_eq!(
            list_warnings(&format!("isolcpus: {raw}\n{QUIET}"), Some(2), Some(3)),
            [ignored(
                "isolcpus",
                raw,
                "feeder core 2 and hot core 3 are not isolated"
            )],
            "{raw}"
        );
        let f = file_with_cores(&format!("isolcpus: {raw}\n{QUIET}"), "active: 0");
        assert_eq!(f.isolated_cpus(), Some(Vec::new()), "{raw}");
    }
}

/// Lists the kernel refuses on any host (Linux v6.8 lib/bitmap-str.c,
/// bitmap_parse_region and bitmap_check_region; the same code is in v5.15,
/// v6.1 and v6.12): a part that starts with no digit, `N` or `all`, a
/// descending range, a group of 0, a number that does not fit 32 bits, a
/// sign, a stride cut short.
const REFUSED: [&str; 10] = [
    "2-x",
    "x",
    "3-2",
    "2,7-4",
    "0-7:1/0",
    "+2",
    "2-",
    "0-7:1",
    "0-7/2",
    "2-3,4294967296",
];

#[test]
fn a_refused_rcu_nocbs_offloads_every_cpu() {
    // kernel/rcu/tree_nocb.h rcu_nocb_setup: on a cpulist_parse error it
    // prints "rcu_nocbs= bad CPU range, all CPUs set" and calls
    // cpumask_setall. One refused part refuses the whole list.
    for raw in REFUSED {
        assert_eq!(
            list_warnings(
                &format!("isolcpus: 2-3\nnohz_full: not set\nrcu_nocbs: {raw}"),
                Some(2),
                Some(3)
            ),
            [
                "nohz_full is not set: feeder core 2 and hot core 3 are not tickless".to_owned(),
                rcu_refused(raw)
            ],
            "{raw}"
        );
    }
    // With nohz_full unread, a refused rcu_nocbs still settles the offload.
    assert_eq!(
        list_warnings(
            "isolcpus: 2-3\nnohz_full: 2-N\nrcu_nocbs: 3-2",
            Some(2),
            Some(3)
        ),
        [unread("nohz_full", "2-N"), rcu_refused("3-2")]
    );
    // No pinned core, no line.
    assert_eq!(
        list_warnings("isolcpus: 2-3\nnohz_full: 2-3\nrcu_nocbs: 3-2", None, None),
        Vec::<String>::new()
    );
}

#[test]
fn a_refused_nohz_full_or_isolcpus_is_ignored_by_the_kernel() {
    // kernel/sched/isolation.c housekeeping_setup: on a cpulist_parse error
    // it prints "Housekeeping: nohz_full= or isolcpus= incorrect CPU range"
    // and sets nothing.
    for raw in REFUSED {
        assert_eq!(
            list_warnings(
                &format!("isolcpus: 2-3\nnohz_full: {raw}\nrcu_nocbs: all"),
                Some(2),
                Some(3)
            ),
            [ignored(
                "nohz_full",
                raw,
                "feeder core 2 and hot core 3 are not tickless"
            )],
            "{raw}"
        );
        // The roles and cores are the pinned ones.
        assert_eq!(
            list_warnings(
                &format!("isolcpus: 2-3\nnohz_full: {raw}\nrcu_nocbs: all"),
                None,
                Some(5)
            ),
            [
                "hot core 5 is not in isolcpus (2-3)".to_owned(),
                ignored("nohz_full", raw, "hot core 5 is not tickless")
            ],
            "{raw}"
        );
    }
    // In isolcpus a part starting with a letter is a flag, so only the
    // lists that start with a digit or a sign reach cpulist_parse.
    for raw in ["3-2", "2-x", "+2", "domain,3-2"] {
        assert_eq!(
            list_warnings(&format!("isolcpus: {raw}\n{QUIET}"), Some(4), None),
            [ignored("isolcpus", raw, "feeder core 4 is not isolated")],
            "{raw}"
        );
        let f = file_with_cores(&format!("isolcpus: {raw}"), "active: 0");
        assert_eq!(f.isolated_cpus(), Some(Vec::new()), "{raw}");
    }
    // A refused part wins over a part only the host can settle.
    assert_eq!(
        list_warnings(
            "isolcpus: 2-3\nnohz_full: 2-N,x\nrcu_nocbs: all",
            Some(2),
            None
        ),
        [ignored(
            "nohz_full",
            "2-N,x",
            "feeder core 2 is not tickless"
        )]
    );
}

#[test]
fn a_refused_nohz_full_offloads_no_core_so_rcu_nocbs_alone_decides() {
    // A refused nohz_full sets no CPU, so it implies no RCU offload: the
    // reading is known, not undetermined.
    assert_eq!(
        list_warnings(
            "isolcpus: 2-3\nnohz_full: 3-2\nrcu_nocbs: 2",
            Some(2),
            Some(3)
        ),
        [
            ignored(
                "nohz_full",
                "3-2",
                "feeder core 2 and hot core 3 are not tickless"
            ),
            "hot core 3 is not in rcu_nocbs (2)".to_owned(),
        ]
    );
    assert_eq!(
        list_warnings("isolcpus: 2-3\nnohz_full: 3-2\nrcu_nocbs: not set", Some(2), Some(3)),
        [
            ignored(
                "nohz_full",
                "3-2",
                "feeder core 2 and hot core 3 are not tickless"
            ),
            "rcu_nocbs is not set: feeder core 2 and hot core 3 are not offloaded from RCU callbacks"
                .to_owned(),
        ]
    );
    // A refused nohz_full names no CPU 0 either.
    assert_eq!(
        list_warnings(
            "isolcpus: 0-3\nnohz_full: 0,x\nrcu_nocbs: all",
            Some(0),
            None
        ),
        [ignored("nohz_full", "0,x", "feeder core 0 is not tickless")]
    );
}

#[test]
fn a_list_only_the_host_can_settle_stays_unreadable() {
    // `N` is the last CPU, so whether `N-2` descends or `0-N:1/N` holds
    // depends on the host's CPU count; `all` with a stride likewise.
    for raw in ["N-2", "2,N", "0-N:1/N", "all:1/2", "0-7:1/2N"] {
        assert_eq!(
            list_warnings(
                &format!("isolcpus: 2-3\nnohz_full: all\nrcu_nocbs: {raw}"),
                Some(2),
                Some(3)
            ),
            Vec::<String>::new(),
            "{raw}"
        );
        assert_eq!(
            list_warnings(
                &format!("isolcpus: 2-3\nnohz_full: {raw}\nrcu_nocbs: all"),
                Some(2),
                Some(3)
            ),
            [unread("nohz_full", raw)],
            "{raw}"
        );
    }
    // A stride no host accepts is refused even beside `N`.
    for raw in ["0-N:3/2", "0-N:1/0", "Nx", "all:x"] {
        assert_eq!(
            list_warnings(
                &format!("isolcpus: 2-3\nnohz_full: {raw}\nrcu_nocbs: all"),
                Some(2),
                None
            ),
            [ignored("nohz_full", raw, "feeder core 2 is not tickless")],
            "{raw}"
        );
    }
}

#[test]
fn an_unknown_isolcpus_flag_with_its_comma_is_skipped() {
    // housekeeping_isolcpus_setup: a word of letters and underscores that is
    // none of the three flags is skipped up to its comma ("isolcpus: Skipped
    // unknown flag") and the kernel reads on. `all` and `N` start with a
    // letter, so before the list they are such words too.
    for (raw, cpus) in [
        ("foo,2-3", vec![2, 3]),
        ("domain,some_flag,managed_irq,2", vec![2]),
        ("all,2", vec![2]),
        ("N,3", vec![3]),
        ("foo,", vec![]),
        ("domain,", vec![]),
        ("nohz,domain,managed_irq,", vec![]),
    ] {
        let f = file_with_cores(&format!("isolcpus: {raw}\n{QUIET}"), "active: 0");
        assert_eq!(f.isolated_cpus(), Some(cpus), "{raw}");
    }
    assert_eq!(
        list_warnings(&format!("isolcpus: foo,2\n{QUIET}"), Some(2), Some(3)),
        ["hot core 3 is not in isolcpus (foo,2)"]
    );
}

#[test]
fn an_isolcpus_flag_with_a_character_no_flag_has_is_refused() {
    // housekeeping_isolcpus_setup: a word in flag position holding anything
    // but letters and underscores is "isolcpus: Invalid flag"; the function
    // returns 0 and nothing is isolated. The check runs before the end of
    // the word matters, so it holds for a last word too.
    for raw in ["no2hz,2-3", "domain,a-b,2", "x9", "domain,foo=1"] {
        assert_eq!(
            list_warnings(&format!("isolcpus: {raw}\n{QUIET}"), Some(2), Some(3)),
            [ignored(
                "isolcpus",
                raw,
                "feeder core 2 and hot core 3 are not isolated"
            )],
            "{raw}"
        );
        let f = file_with_cores(&format!("isolcpus: {raw}"), "active: 0");
        assert_eq!(f.isolated_cpus(), Some(Vec::new()), "{raw}");
    }
}

#[test]
fn a_last_isolcpus_word_without_its_comma_is_not_read() {
    // The kernel matches each flag with its comma, so a last word (a known
    // flag, an unknown one, `all`) falls into the unknown-flag loop, whose
    // `str++` then steps past the end of the value. Source alone does not
    // settle what follows, so the harness says unreadable and does not guess.
    for raw in [
        "domain",
        "nohz,domain",
        "foo",
        "domain,foo",
        "all",
        "domain,all",
        "N",
    ] {
        assert_eq!(
            list_warnings(&format!("isolcpus: {raw}\n{QUIET}"), Some(2), Some(3)),
            [unread("isolcpus", raw)],
            "{raw}"
        );
        let f = file_with_cores(&format!("isolcpus: {raw}"), "active: 0");
        assert_eq!(f.isolated_cpus(), None, "{raw}");
    }
}
