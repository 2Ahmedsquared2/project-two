//! The report: what ran, where, on which commit, and the percentiles.

use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::arms::{Arm, ArmReport, Run};
use crate::stage::{active_parser_mode, stage_table, ParserMode};
use crate::{HostFactsNote, LatencyHist, ReplayError};

/// The commit this harness was built from (`git rev-parse HEAD` at build
/// time, with `-dirty` appended when tracked files had uncommitted changes).
pub const GIT_SHA: &str = env!("HL_REPLAY_GIT_SHA");

/// The host a run happened on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostInfo {
    /// Kernel release (`/proc/sys/kernel/osrelease`).
    pub kernel: String,
    /// CPU model name (`/proc/cpuinfo`).
    pub cpu_model: String,
    /// Cores this process may use.
    pub cores: usize,
}

impl HostInfo {
    /// Reads the host's details; a field that cannot be read says `unknown`.
    #[must_use]
    pub fn detect() -> Self {
        let read = |p: &str| std::fs::read_to_string(p).ok();
        let kernel = read("/proc/sys/kernel/osrelease")
            .map_or_else(|| "unknown".to_owned(), |s| s.trim().to_owned());
        let cpu_model = read("/proc/cpuinfo")
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("model name"))
                    .and_then(|l| l.split_once(':'))
                    .map(|(_, v)| v.trim().to_owned())
            })
            .unwrap_or_else(|| "unknown".to_owned());
        let cores = std::thread::available_parallelism().map_or(0, std::num::NonZero::get);
        Self {
            kernel,
            cpu_model,
            cores,
        }
    }
}

/// Everything about a run that is not a measurement.
#[derive(Debug, Clone)]
pub struct RunMeta {
    /// The commit; normally [`GIT_SHA`].
    pub commit: String,
    /// The host.
    pub host: HostInfo,
    /// Core the feeder and sink were pinned to, if any.
    pub feeder_core: Option<usize>,
    /// Core the hot thread was pinned to, if any.
    pub hot_core: Option<usize>,
    /// Core the `compio-sqpoll` arm's kernel polling thread was given, if any.
    pub sqpoll_core: Option<usize>,
    /// Rounds per arm.
    pub rounds: u32,
    /// Recorded lines per round.
    pub measured_per_round: u64,
    /// Unrecorded warmup lines per round.
    pub warmup_per_round: u64,
    /// Time between lines.
    pub interval: Duration,
    /// What was replayed, in words.
    pub input: String,
    /// The host-facts capture this run carries, if any.
    pub host_facts: HostFactsNote,
}

/// The five metrics of an arm, headline first: file-name-safe label and histogram.
fn metrics(a: &ArmReport) -> [(&'static str, &LatencyHist); 5] {
    [
        ("sched-t2", &a.hists.sched_total),
        ("t2-t0", &a.hists.e2e),
        ("t1-t0", &a.hists.parse),
        ("feed-t0", &a.hists.detect),
        ("feed-t2", &a.hists.total),
    ]
}

/// The `host facts:` lines: which capture a run carries, or that it has none.
fn render_host_facts(s: &mut String, note: &HostFactsNote) {
    match note {
        HostFactsNote::NotRequired => {
            let _ = writeln!(s, "host facts: not captured (nothing written to disk)");
        }
        HostFactsNote::Waived => {
            let _ = writeln!(
                s,
                "host facts: NOT CAPTURED (--allow-missing-host-facts): local run, not a result for a write-up"
            );
        }
        HostFactsNote::Captured {
            path,
            captured_utc,
            unavailable,
            manual_rtt,
            warnings,
        } => {
            let gaps = if unavailable.is_empty() {
                "none".to_owned()
            } else {
                unavailable.join(", ")
            };
            let _ = writeln!(
                s,
                "host facts: {path}, captured {captured_utc}, {} sections checked, unavailable in: {gaps}",
                crate::HostFacts::REQUIRED.len()
            );
            let _ = writeln!(
                s,
                "  rtt-manual: {} (operator step, never checked by the harness)",
                if *manual_rtt { "present" } else { "absent" }
            );
            for w in warnings {
                let _ = writeln!(s, "  warning: {w}");
            }
        }
    }
}

/// Renders the plain-text report: commit, host, pinning, stage labels and one
/// row per arm and metric with the sample count, p50, p90, p99, p99.9, p99.99
/// and max in nanoseconds. Percentiles only: latency is not summarised by a
/// single central number.
#[must_use]
pub fn render_report(meta: &RunMeta, arms: &[ArmReport]) -> String {
    let mut s = String::new();
    // Writing to a `String` cannot fail.
    let _ = writeln!(s, "hl-replay: line seen -> bytes handed to the socket");
    let _ = writeln!(s, "commit: {}", meta.commit);
    let _ = writeln!(s, "kernel: {}", meta.host.kernel);
    let _ = writeln!(s, "cpu: {}", meta.host.cpu_model);
    let _ = writeln!(s, "cores: {}", meta.host.cores);
    render_host_facts(&mut s, &meta.host_facts);
    let core = |c: Option<usize>| c.map_or_else(|| "unpinned".to_owned(), |c| format!("core {c}"));
    if meta.feeder_core.is_none() && meta.hot_core.is_none() && meta.sqpoll_core.is_none() {
        let _ = writeln!(s, "pinning: none");
    } else {
        let _ = writeln!(
            s,
            "pinning: feeder and sink {}, hot thread {}, sqpoll thread {} (tokio-multi-thread is never pinned; sqpoll thread = the kernel polling thread of compio-sqpoll)",
            core(meta.feeder_core),
            core(meta.hot_core),
            core(meta.sqpoll_core)
        );
    }
    let parser = stage_table()[1];
    let _ = writeln!(s, "parser: {} ({})", parser.name, active_parser_mode());
    let _ = writeln!(
        s,
        "complete lines: enforced by type (hl_latency::CompleteLine, made only where the reader has seen the newline)"
    );
    let _ = writeln!(s, "input: {}", meta.input);
    let _ = writeln!(
        s,
        "rounds: {} per arm, interleaved; {} recorded + {} warmup lines per round; one line every {} us (open loop)",
        meta.rounds,
        meta.measured_per_round,
        meta.warmup_per_round,
        meta.interval.as_micros()
    );
    let _ = writeln!(s, "stages:");
    for st in stage_table() {
        let _ = writeln!(s, "  {}={} ({})", st.stage, st.label, st.name);
    }
    let _ = writeln!(
        s,
        "clock: one monotonic clock (std::time::Instant) for every stamp"
    );
    let _ = writeln!(
        s,
        "  sched = when the line was due: start + i * interval (a late feeder stays in the number)"
    );
    let _ = writeln!(
        s,
        "  feed  = feeder, just before it actually writes the line"
    );
    let _ = writeln!(s, "  t0   = hot thread, read() returned the line's bytes");
    let _ = writeln!(s, "  t1   = parser returned");
    let _ = writeln!(
        s,
        "  t2   = write of the signed order to the sink socket returned"
    );
    let _ = writeln!(
        s,
        "headline: sched-t2 (includes the wake-up, which is what the arms differ on); feed-t2 beside it shows how much is the feeder's own lateness; t2-t0 is in-process; feed-t0 is detection"
    );
    let _ = writeln!(s, "unit: nanoseconds; rounds are merged histograms");
    match active_parser_mode() {
        ParserMode::Strict => {
            let _ = writeln!(
                s,
                "scan-vs-strict: not applicable, the active parser is strict (it is its own validator)"
            );
        }
        ParserMode::HeaderScan => {
            let _ = writeln!(
                s,
                "scan-vs-strict: every 16th line is re-parsed strictly after t2; any mismatch fails the run; fallbacks = lines the scan could not read, timed with the strict parser"
            );
        }
    }
    let _ = writeln!(
        s,
        "unmeasured: last-byte cache miss on real ~100 KB lines (needs the Tokyo run on real node lines)"
    );
    let _ = writeln!(s);
    render_rows(&mut s, arms);
    s
}

/// One row per arm and metric, or the reason an arm could not run.
fn render_rows(s: &mut String, arms: &[ArmReport]) {
    for a in arms {
        if let Some(why) = &a.unavailable {
            let _ = writeln!(s, "{:<21} unavailable: {why}", a.arm.name());
            continue;
        }
        let pinned = match a.hot_pinned {
            Some(true) => "hot=pinned",
            Some(false) => "hot=PIN-REFUSED",
            None => "hot=unpinned",
        };
        // What the kernel reports for the polling thread, not what was asked.
        let pinned = match &a.sqpoll_cpus {
            Some(cpus) => format!("{pinned} sqpoll=cpus:{cpus}"),
            None if a.arm == Arm::CompioSqpoll => format!("{pinned} sqpoll=cpus:unknown"),
            None => pinned.to_owned(),
        };
        for (metric, h) in metrics(a) {
            let m = h.summary();
            let _ = writeln!(
                s,
                "{:<21} {:<8} n={:<8} p50={:<9} p90={:<9} p99={:<9} p99.9={:<10} p99.99={:<10} max={:<11} {pinned} parser={}",
                a.arm.name(),
                metric,
                m.count,
                m.p50,
                m.p90,
                m.p99,
                m.p999,
                m.p9999,
                m.max,
                active_parser_mode(),
            );
        }
        if active_parser_mode() == ParserMode::HeaderScan {
            let _ = writeln!(
                s,
                "{:<21} scan-vs-strict: validated={} mismatches={} fallbacks={}",
                a.arm.name(),
                a.hists.validated,
                a.hists.scan_strict_mismatches,
                a.hists.scan_fallbacks
            );
        }
    }
}

/// [`render_report`] followed by the run's seed and, per round, the order the
/// arms ran in and the kernel load figures before and after (1, 5 and 15 minute,
/// then runnable/total), so a noisy round can be told from a quiet one.
#[must_use]
pub fn render_run(meta: &RunMeta, run: &Run) -> String {
    let mut s = render_report(meta, &run.arms);
    let _ = writeln!(s);
    let _ = writeln!(s, "seed: {} (arm order per round)", run.seed);
    for (i, r) in run.rounds.iter().enumerate() {
        let order: Vec<&str> = r.order.iter().map(|a| a.name()).collect();
        let _ = writeln!(
            s,
            "round {} order: {} | load before: {} | load after: {}",
            i + 1,
            order.join(", "),
            r.load_before,
            r.load_after
        );
    }
    s
}

/// Writes `report.txt` and, for every arm that ran, each metric's raw
/// histogram twice: `<arm>.<metric>.hdr` (HDR V2 binary, mergeable, readable
/// by any `HdrHistogram` library) and `<arm>.<metric>.buckets.txt`
/// (`value_ns count` per line). Returns the paths written.
pub fn write_artifacts(
    dir: &Path,
    report: &str,
    arms: &[ArmReport],
) -> Result<Vec<PathBuf>, ReplayError> {
    std::fs::create_dir_all(dir)?;
    let mut written = Vec::new();
    let path = dir.join("report.txt");
    std::fs::write(&path, report)?;
    written.push(path);
    for a in arms.iter().filter(|a| a.unavailable.is_none()) {
        for (metric, h) in metrics(a) {
            let path = dir.join(format!("{}.{metric}.hdr", a.arm.name()));
            let mut w = BufWriter::new(File::create(&path)?);
            h.write_v2(&mut w)?;
            w.flush()?;
            written.push(path);
            let path = dir.join(format!("{}.{metric}.buckets.txt", a.arm.name()));
            let mut w = BufWriter::new(File::create(&path)?);
            h.write_buckets(&mut w)?;
            writeln!(
                w,
                "# arm={} metric={metric} parser={} commit={GIT_SHA}",
                a.arm.name(),
                active_parser_mode()
            )?;
            w.flush()?;
            written.push(path);
        }
    }
    Ok(written)
}
