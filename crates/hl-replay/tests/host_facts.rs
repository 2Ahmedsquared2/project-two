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
        Vec::<String>::new(),
        "every nohz_full core is offloaded, so rcu_nocbs is not consulted"
    );
    assert_eq!(
        list_warnings(
            "isolcpus: 0-7\nnohz_full: 6-7\nrcu_nocbs: 2,x",
            Some(2),
            None
        ),
        [
            "feeder core 2 is not in nohz_full (6-7)".to_owned(),
            unread("rcu_nocbs", "2,x")
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
        [unread("nohz_full", "domain,2-3")]
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
fn a_cpu_number_no_kernel_can_hold_makes_the_list_unreadable() {
    // The kernel refuses a list naming a CPU at or past its mask width
    // (-ERANGE, Linux v6.8 lib/bitmap-str.c bitmap_check_region), and no
    // x86-64 or arm64 kernel is built wider than 8192 CPUs (v6.8
    // arch/x86/Kconfig NR_CPUS_RANGE_END; arm64 stops at 4096). Such a range
    // is never expanded CPU by CPU.
    let unread = |key: &str, raw: &str| {
        format!("{key} ({raw}) is not a CPU list this harness can read: pinned cores not checked against it")
    };
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
            [unread("nohz_full", raw)],
            "{raw}"
        );
        let f = file_with_cores(&format!("isolcpus: {raw}"), "active: 0");
        assert_eq!(f.isolated_cpus(), None, "{raw}");
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
fn a_stride_using_more_than_its_group_is_unreadable() {
    // The kernel rejects the whole list when "used size" exceeds "group
    // size" (-EINVAL: `r->off > r->group_len`, Linux v6.8 lib/bitmap-str.c
    // bitmap_check_region), so it is not a CPU list to read either.
    let unread = |key: &str, raw: &str| {
        format!("{key} ({raw}) is not a CPU list this harness can read: pinned cores not checked against it")
    };
    for raw in ["0-7:3/2", "2-3,0-7:5/4"] {
        assert_eq!(
            list_warnings(
                &format!("isolcpus: 0-7\nnohz_full: {raw}\nrcu_nocbs: all"),
                Some(2),
                Some(3)
            ),
            [unread("nohz_full", raw)],
            "{raw}"
        );
        let f = file_with_cores(&format!("isolcpus: {raw}"), "active: 0");
        assert_eq!(f.isolated_cpus(), None, "{raw}");
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
