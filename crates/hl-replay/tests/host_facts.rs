//! The host-facts file: what `scripts/capture-host.sh` writes, what the
//! harness requires of it, and what the report says about it.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use hl_replay::{render_report, HostFacts, HostFactsNote, HostInfo, ReplayError, RunMeta};

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/capture-host.sh")
}
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
        s.push_str(&format!("== begin {name}\n{body}\n== end {name}\n"));
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
    assert!(f.unavailable().is_empty());
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

fn capture(dir: &str, path_env: Option<&str>) -> String {
    let out = tmp(dir).join("host-facts.txt");
    let mut c = Command::new(bash());
    c.arg(script()).arg(&out);
    if let Some(p) = path_env {
        c.env("PATH", p);
    }
    let o = c.output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    std::fs::read_to_string(out).unwrap()
}

#[test]
fn the_capture_script_writes_a_file_the_harness_accepts() {
    let text = capture("real", None);
    let f = HostFacts::parse(&text).unwrap();
    f.check_kernel(&HostInfo::detect().kernel).unwrap();
    assert!(!f.has_manual_rtt(), "the script must never write the RTT");
}

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
    let note = HostFactsNote::captured(Path::new("/tmp/facts.txt"), &f);
    assert_eq!(
        note,
        HostFactsNote::Captured {
            path: "/tmp/facts.txt".to_owned(),
            captured_utc: "2026-11-24T02:00:00Z".to_owned(),
            unavailable: vec!["chrony".to_owned()],
            manual_rtt: false,
        }
    );
    let report = render_report(&meta(note), &[]);
    assert!(
        report.contains(
            "host facts: /tmp/facts.txt, captured 2026-11-24T02:00:00Z, 16 sections checked, unavailable in: chrony"
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
