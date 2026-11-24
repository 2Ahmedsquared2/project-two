//! Contract of the `hl-capture` binary.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fmt::Write as _;

use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, ChildStderr, Command, Stdio};
use std::time::Duration;

use hl_latency::{read_observations, Source};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_hl-capture"))
}

/// The line `hl-capture node` writes to stderr once its first poll is done:
/// a file that appears after it is read from its start.
const READY: &str = "{\"ready\":true}";
/// The stderr line the reader prints when it stops reading (deadline or error),
/// before the writer drains the ring.
const READER_STOPPED: &str = "{\"reader_stopped\":true}";

/// Spawns `cmd` with stderr piped and blocks until it reports [`READY`], so
/// a test never races the binary's startup (a slow `exec` on a loaded Mac
/// once made the first poll see the burst as history and skip it). Also
/// returns what stderr held before that line (the `detect` line).
fn spawn_ready(cmd: &mut Command) -> (Child, BufReader<ChildStderr>, String) {
    let mut child = cmd.stderr(Stdio::piped()).spawn().unwrap();
    let mut err = BufReader::new(child.stderr.take().unwrap());
    let mut before = String::new();
    let mut line = String::new();
    while err.read_line(&mut line).unwrap() > 0 && line.trim_end() != READY {
        before.push_str(&line);
        line.clear();
    }
    if line.trim_end() != READY {
        let status = child.wait().unwrap();
        panic!("stderr ended before the ready line ({status}): {before}");
    }
    (child, err, before)
}

/// Waits for a [`spawn_ready`] child and returns its status and its whole
/// stderr apart from the ready line.
fn finish(
    mut child: Child,
    mut err: BufReader<ChildStderr>,
    mut stderr: String,
) -> (std::process::ExitStatus, Vec<u8>) {
    err.read_to_string(&mut stderr).unwrap();
    (child.wait().unwrap(), stderr.into_bytes())
}

/// A first poll that takes longer than the old fixed 200 ms sleep must not
/// make the test skip the burst: the ready line comes after the first poll,
/// however late that is.
#[test]
fn node_arm_reports_ready_after_its_first_poll() {
    let t = tempfile::tempdir().unwrap();
    let (child, mut err, before) = spawn_ready(
        bin()
            .arg("node")
            .arg(t.path())
            .arg("--out")
            .arg(t.path().join("obs.jsonl"))
            .args(["--duration-s", "1", "--poll-us", "1000"]),
    );
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let mut rest = String::new();
    err.read_to_string(&mut rest).unwrap();
    assert!(rest.contains("\"reads\":"), "counters follow: {rest}");
    // Only the detect announcement may come before the ready line.
    assert_eq!(before.lines().count(), 1, "{before}");
    assert!(before.starts_with("{\"detect\""), "{before}");
}

#[test]
fn usage_errors_exit_2() {
    assert_eq!(bin().output().unwrap().status.code(), Some(2));
    assert_eq!(bin().arg("bogus").output().unwrap().status.code(), Some(2));
    assert_eq!(
        bin()
            .args(["node", "/tmp", "--out"])
            .output()
            .unwrap()
            .status
            .code(),
        Some(2)
    );
}

#[test]
fn node_arm_captures_lines_from_a_file_that_appears_during_the_run() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("node_raw_book_diffs");
    let out = t.path().join("obs.jsonl");
    let (mut child, _err, _pre) =
        spawn_ready(bin().arg("node").arg(&root).arg("--out").arg(&out).args([
            "--duration-s",
            "2",
            "--poll-us",
            "1000",
        ]));
    let dir = root.join("hourly").join("20261119");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("8"),
        "{\"local_time\":\"2026-11-19T08:00:00.071\",\"block_time\":\"2026-11-19T08:00:00.070\",\"block_number\":2,\"events\":[]}\n\
         {\"local_time\":\"2026-11-19T08:00:00.141\",\"block_time\":\"2026-11-19T08:00:00.140\",\"block_number\":3,\"events\":[]}\n",
    )
    .unwrap();
    let status = child.wait().unwrap();
    assert!(status.success());
    let obs = read_observations(std::io::BufReader::new(fs::File::open(&out).unwrap())).unwrap();
    let heights: Vec<_> = obs.iter().map(|o| o.block_number.unwrap()).collect();
    assert_eq!(heights, vec![2, 3]);
    assert!(obs
        .iter()
        .all(|o| o.source == Source::Node && o.recv_ns > 0));
}

#[test]
fn duration_bounds_the_run_even_with_a_long_poll() {
    let t = tempfile::tempdir().unwrap();
    let start = std::time::Instant::now();
    let status = bin()
        .arg("node")
        .arg(t.path())
        .arg("--out")
        .arg(t.path().join("obs.jsonl"))
        .args([
            "--duration-s",
            "1",
            "--detect",
            "poll",
            "--poll-us",
            "3000000",
        ])
        .status()
        .unwrap();
    let took = start.elapsed();
    assert!(status.success());
    assert!(took < Duration::from_millis(1800), "ran {took:?}");
}

#[test]
fn max_line_bytes_flag_is_accepted() {
    let t = tempfile::tempdir().unwrap();
    let status = bin()
        .arg("node")
        .arg(t.path())
        .arg("--out")
        .arg(t.path().join("obs.jsonl"))
        .args(["--duration-s", "0", "--max-line-bytes", "4096"])
        .status()
        .unwrap();
    assert!(status.success());
}

/// A busy input must not hold the run past `--duration-s`: the deadline is
/// checked between bounded batches, not only around a whole drain.
#[test]
fn duration_bounds_the_run_under_a_busy_input() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("node_raw_book_diffs");
    let dir = root.join("hourly").join("20261119");
    fs::create_dir_all(&dir).unwrap();
    // 600,000 valid lines, staged outside the root and renamed in at once.
    let mut body = String::with_capacity(600_000 * 120);
    for h in 0..600_000u64 {
        writeln!(
            body,
            "{{\"local_time\":\"2026-11-19T08:00:00.071\",\"block_time\":\"2026-11-19T08:00:00.070\",\"block_number\":{h},\"events\":[]}}"
        )
        .unwrap();
    }
    let staged = t.path().join("staged");
    fs::write(&staged, body).unwrap();
    let out = t.path().join("obs.jsonl");
    let start = std::time::Instant::now();
    let (mut child, mut err, _pre) = spawn_ready(
        bin()
            .arg("node")
            .arg(&root)
            .arg("--out")
            .arg(&out)
            // A ring that holds the whole burst: nothing can be dropped, so the
            // output must be a gap-free prefix.
            .args(["--duration-s", "1", "--poll-us", "1000"])
            .args(["--ring-records", "1048576"]),
    );
    fs::rename(&staged, dir.join("8")).unwrap();
    // The deadline bounds reading. What the ring already holds is still
    // written afterwards, and that drain takes as long as its backlog needs,
    // so the bound is measured at the reader's stop line, not at exit.
    let mut line = String::new();
    while err.read_line(&mut line).unwrap() > 0 && line.trim_end() != READER_STOPPED {
        line.clear();
    }
    let took = start.elapsed();
    assert_eq!(
        line.trim_end(),
        READER_STOPPED,
        "the reader reports its stop"
    );
    assert!(took < Duration::from_millis(1800), "read for {took:?}");
    let mut rest = String::new();
    err.read_to_string(&mut rest).unwrap();
    let status = child.wait().unwrap();
    assert!(status.success());
    // Everything read before the deadline is written: the drain is lossless.
    let c = counters(rest.as_bytes());
    assert_eq!(c["ring_dropped"], 0);
    assert_eq!(c["written"], c["lines"]);
    let obs = read_observations(std::io::BufReader::new(fs::File::open(&out).unwrap())).unwrap();
    let heights: Vec<_> = obs.iter().map(|o| o.block_number.unwrap()).collect();
    assert!(!heights.is_empty(), "some of the burst was captured");
    assert_eq!(
        heights,
        (0..u64::try_from(heights.len()).unwrap()).collect::<Vec<_>>(),
        "a prefix, in order"
    );
}

// ---- ring to disk, counters ---------------------------------------------------

/// The counters object the binary prints on stderr at exit.
fn counters(stderr: &[u8]) -> serde_json::Value {
    let text = String::from_utf8_lossy(stderr);
    let line = text
        .lines()
        .rev()
        .find(|l| l.starts_with("{\"lines\""))
        .unwrap_or_else(|| panic!("no counters line in: {text}"));
    serde_json::from_str(line).unwrap()
}

fn burst(lines: u64) -> String {
    let mut body = String::new();
    for h in 0..lines {
        writeln!(
            body,
            "{{\"local_time\":\"2026-11-19T08:00:00.071\",\"block_time\":\"2026-11-19T08:00:00.070\",\"block_number\":{h},\"events\":[]}}"
        )
        .unwrap();
    }
    body
}

#[test]
fn counters_report_what_was_written_and_what_qualifies_it() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("node_raw_book_diffs");
    let dir = root.join("hourly").join("20261119");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("8"), "").unwrap();
    let out = t.path().join("obs.jsonl");
    let (child, err, pre) =
        spawn_ready(bin().arg("node").arg(&root).arg("--out").arg(&out).args([
            "--duration-s",
            "1",
            "--poll-us",
            "1000",
            "--rescan-ms",
            "5",
        ]));
    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(dir.join("8"))
        .unwrap();
    std::io::Write::write_all(&mut f, burst(3).as_bytes()).unwrap();
    let (status, stderr) = finish(child, err, pre);
    assert!(status.success());
    let c = counters(&stderr);
    for (key, want) in [
        ("lines", 3),
        ("written", 3),
        ("ring_dropped", 0),
        ("scan_fallbacks", 0),
        ("bad_lines", 0),
        ("rotations", 0),
        ("post_rotation_lines", 0),
        ("height_breaks", 0),
        ("height_repeats", 0),
        ("height_gaps", 0),
        ("height_backwards", 0),
    ] {
        assert_eq!(c[key], want, "{key} in {c}");
    }
    assert!(c["rescans"].as_u64().unwrap() >= 2, "timer rescans: {c}");
    let obs = read_observations(std::io::BufReader::new(fs::File::open(&out).unwrap())).unwrap();
    assert_eq!(obs.len(), 3);
}

/// Full-ring policy: the newest record is dropped and counted; the reader
/// is never blocked, so everything that was written is in order and the
/// counters add up.
#[test]
fn a_full_ring_drops_the_newest_and_counts_instead_of_blocking() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("node_raw_book_diffs");
    let dir = root.join("hourly").join("20261119");
    fs::create_dir_all(&dir).unwrap();
    let staged = t.path().join("staged");
    fs::write(&staged, burst(200_000)).unwrap();
    let out = t.path().join("obs.jsonl");
    let (child, err, pre) =
        spawn_ready(bin().arg("node").arg(&root).arg("--out").arg(&out).args([
            "--duration-s",
            "2",
            "--poll-us",
            "1000",
            "--ring-records",
            "8",
        ]));
    fs::rename(&staged, dir.join("8")).unwrap();
    let (status, stderr) = finish(child, err, pre);
    assert!(status.success());
    let c = counters(&stderr);
    let (lines, written, dropped) = (
        c["lines"].as_u64().unwrap(),
        c["written"].as_u64().unwrap(),
        c["ring_dropped"].as_u64().unwrap(),
    );
    assert_eq!(lines, 200_000, "the reader was not held back: {c}");
    assert!(dropped > 0, "an 8-record ring cannot hold the burst: {c}");
    assert_eq!(written + dropped, lines, "{c}");
    let obs = read_observations(std::io::BufReader::new(fs::File::open(&out).unwrap())).unwrap();
    assert_eq!(u64::try_from(obs.len()).unwrap(), written);
    let heights: Vec<_> = obs.iter().map(|o| o.block_number.unwrap()).collect();
    assert!(heights.windows(2).all(|w| w[0] < w[1]), "order is kept");
}

#[test]
fn ring_and_rescan_flags_reject_zero() {
    for flag in ["--ring-records", "--rescan-ms"] {
        let status = bin()
            .args(["node", "/tmp", "--out", "/dev/null", "--duration-s", "0"])
            .args([flag, "0"])
            .output()
            .unwrap()
            .status;
        assert_eq!(status.code(), Some(2), "{flag} 0");
    }
}

// ---- detect modes -------------------------------------------------------------

/// The `{"detect":...}` line the binary prints on stderr at start.
fn detect_line(stderr: &[u8]) -> serde_json::Value {
    let text = String::from_utf8_lossy(stderr);
    let line = text
        .lines()
        .find(|l| l.starts_with("{\"detect\""))
        .unwrap_or_else(|| panic!("no detect line in: {text}"));
    serde_json::from_str::<serde_json::Value>(line).unwrap()["detect"].clone()
}

/// Runs the binary for a second in `mode` (`None`: no `--detect` flag)
/// against a file that gets three lines, and returns its stderr.
fn run_detect(mode: Option<&str>) -> Vec<u8> {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("node_raw_book_diffs");
    let dir = root.join("hourly").join("20261119");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("8"), "").unwrap();
    let out = t.path().join("obs.jsonl");
    let mut cmd = bin();
    cmd.arg("node").arg(&root).arg("--out").arg(&out);
    cmd.args(["--duration-s", "1", "--rescan-ms", "50"]);
    if let Some(mode) = mode {
        cmd.args(["--detect", mode]);
    }
    let (child, err, pre) = spawn_ready(&mut cmd);
    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(dir.join("8"))
        .unwrap();
    std::io::Write::write_all(&mut f, burst(3).as_bytes()).unwrap();
    // A new hourly file mid-run: every mode must find it.
    std::thread::sleep(Duration::from_millis(100));
    fs::write(dir.join("9"), burst(2)).unwrap();
    let (status, stderr) = finish(child, err, pre);
    assert!(status.success(), "{mode:?}");
    let c = counters(&stderr);
    assert_eq!(
        (c["lines"].as_u64(), c["written"].as_u64()),
        (Some(5), Some(5)),
        "{mode:?}: {c}"
    );
    assert_eq!(c["rotations"], 1, "{mode:?}: {c}");
    let obs = read_observations(std::io::BufReader::new(fs::File::open(&out).unwrap())).unwrap();
    let heights: Vec<_> = obs.iter().map(|o| o.block_number.unwrap()).collect();
    assert_eq!(heights, vec![0, 1, 2, 0, 1], "{mode:?}");
    stderr
}

fn cpus() -> usize {
    std::thread::available_parallelism().map_or(1, std::num::NonZero::get)
}

#[test]
fn busy_poll_is_the_default_and_the_live_mode_is_announced() {
    let d = detect_line(&run_detect(None));
    assert_eq!(d["requested"], "busy-poll");
    // One CPU cannot spin and write: the documented fallback is `poll`.
    let live = if cpus() >= 2 { "busy-poll" } else { "poll" };
    assert_eq!(d["live"], live, "{d}");
    assert_eq!(d["fallback"].is_null(), cpus() >= 2, "{d}");
}

#[test]
fn poll_mode_is_announced() {
    let d = detect_line(&run_detect(Some("poll")));
    assert_eq!(
        (&d["requested"], &d["live"]),
        (&"poll".into(), &"poll".into())
    );
    assert!(d["fallback"].is_null(), "{d}");
}

#[cfg(target_os = "linux")]
#[test]
fn inotify_mode_is_live_on_linux() {
    let d = detect_line(&run_detect(Some("inotify")));
    assert_eq!(
        (&d["requested"], &d["live"]),
        (&"inotify".into(), &"inotify".into())
    );
    assert!(d["fallback"].is_null(), "{d}");
}

/// Off Linux there is no inotify: the run falls back to `poll` and says so.
#[cfg(not(target_os = "linux"))]
#[test]
fn inotify_mode_falls_back_to_poll_off_linux() {
    let d = detect_line(&run_detect(Some("inotify")));
    assert_eq!(
        (&d["requested"], &d["live"]),
        (&"inotify".into(), &"poll".into())
    );
    assert!(d["fallback"].as_str().unwrap().contains("Linux"), "{d}");
}

#[test]
fn an_unknown_detect_mode_is_a_usage_error() {
    let status = bin()
        .args(["node", "/tmp", "--out", "/dev/null", "--detect", "epoll"])
        .output()
        .unwrap()
        .status;
    assert_eq!(status.code(), Some(2));
}

/// The default rescan interval is coarse (1 s), but a run that follows no
/// file yet looks for one every 10 ms: a file that appears is picked up
/// long before the first coarse rescan.
#[test]
fn the_first_file_is_found_quickly_whatever_the_rescan_interval() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("node_raw_book_diffs");
    let out = t.path().join("obs.jsonl");
    let (child, err, pre) =
        spawn_ready(bin().arg("node").arg(&root).arg("--out").arg(&out).args([
            "--duration-s",
            "1",
            "--detect",
            "poll",
            "--rescan-ms",
            "60000",
        ]));
    let dir = root.join("hourly").join("20261119");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("8"), burst(2)).unwrap();
    let (status, stderr) = finish(child, err, pre);
    assert!(status.success());
    let c = counters(&stderr);
    assert_eq!(c["written"], 2, "{c}");
}

// ---- signals (issue #28) ------------------------------------------------------

#[cfg(unix)]
mod signals {
    use super::*;
    use std::time::Instant;

    fn send(child: &Child, signal: &str) {
        let status = Command::new("kill")
            .arg(format!("-{signal}"))
            .arg(child.id().to_string())
            .status()
            .unwrap();
        assert!(status.success(), "kill -{signal}");
    }

    /// Kills the child with SIGKILL if the test has not finished with it in
    /// 60 s, so a run that ignores its signal fails the test instead of
    /// hanging it. Dropping the guard calls the watchdog off.
    struct Watchdog(std::sync::mpsc::Sender<()>);

    impl Drop for Watchdog {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }

    fn watchdog(child: &Child) -> Watchdog {
        let (tx, rx) = std::sync::mpsc::channel();
        let pid = child.id().to_string();
        std::thread::spawn(move || {
            if rx.recv_timeout(Duration::from_secs(60))
                == Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            {
                let _ = Command::new("kill").args(["-KILL", &pid]).status();
            }
        });
        Watchdog(tx)
    }

    /// Complete lines in `path` so far (0 while it does not exist).
    fn lines_in(path: &std::path::Path) -> usize {
        fs::read(path).map_or(0, |b| memchr::memchr_iter(b'\n', &b).count())
    }

    /// Reads stderr up to and including `want`; returns what came before it.
    fn read_until(err: &mut BufReader<ChildStderr>, want: &str) -> String {
        let (mut before, mut line) = (String::new(), String::new());
        while err.read_line(&mut line).unwrap() > 0 && line.trim_end() != want {
            before.push_str(&line);
            line.clear();
        }
        assert_eq!(line.trim_end(), want, "stderr ended first: {before}");
        before
    }

    /// A run with no `--duration-s`, stopped by `signal` while the writer
    /// still has a backlog: it exits 0, everything the reader pushed is in
    /// the file, and the counters line is there and says which signal.
    fn stops_cleanly_on(signal: &str, number: u64) {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("node_raw_book_diffs");
        let dir = root.join("hourly").join("20261119");
        fs::create_dir_all(&dir).unwrap();
        let staged = t.path().join("staged");
        fs::write(&staged, burst(300_000)).unwrap();
        let out = t.path().join("obs.jsonl");
        let (mut child, mut err, _pre) = spawn_ready(
            bin()
                .arg("node")
                .arg(&root)
                .arg("--out")
                .arg(&out)
                .args(["--detect", "poll", "--poll-us", "1000"])
                .args(["--ring-records", "1048576"]),
        );
        let _guard = watchdog(&child);
        fs::rename(&staged, dir.join("8")).unwrap();
        // Signal as soon as the writer has started on the burst.
        let start = Instant::now();
        while lines_in(&out) == 0 {
            assert!(start.elapsed() < Duration::from_secs(20), "nothing written");
            std::thread::sleep(Duration::from_millis(1));
        }
        send(&child, signal);
        let mut rest = String::new();
        err.read_to_string(&mut rest).unwrap();
        let status = child.wait().unwrap();
        assert_eq!(status.code(), Some(0), "{status}: {rest}");
        assert!(rest.contains(READER_STOPPED), "{rest}");
        let c = counters(rest.as_bytes());
        assert_eq!(c["stop_signal"], number, "{c}");
        assert_eq!(c["ring_dropped"], 0, "{c}");
        assert_eq!(c["written"], c["lines"], "the ring was drained: {c}");
        let written = c["written"].as_u64().unwrap();
        assert!(written > 0, "{c}");
        let obs =
            read_observations(std::io::BufReader::new(fs::File::open(&out).unwrap())).unwrap();
        let heights: Vec<_> = obs.iter().map(|o| o.block_number.unwrap()).collect();
        assert_eq!(
            heights,
            (0..written).collect::<Vec<_>>(),
            "every record pushed is in the file, in order"
        );
    }

    #[test]
    fn sigterm_stops_reading_drains_the_ring_and_prints_the_counters() {
        stops_cleanly_on("TERM", 15);
    }

    #[test]
    fn sigint_stops_reading_drains_the_ring_and_prints_the_counters() {
        stops_cleanly_on("INT", 2);
    }

    /// A run that ends at its deadline says no signal stopped it.
    #[test]
    fn a_run_that_ends_at_its_deadline_reports_no_stop_signal() {
        let t = tempfile::tempdir().unwrap();
        let out = bin()
            .arg("node")
            .arg(t.path())
            .arg("--out")
            .arg(t.path().join("obs.jsonl"))
            .args(["--duration-s", "0"])
            .output()
            .unwrap();
        assert!(out.status.success());
        let c = counters(&out.stderr);
        assert!(c["stop_signal"].is_null(), "{c}");
        assert!(c.get("stop_signal").is_some(), "the field is present: {c}");
    }

    /// A sleeping reader notices the signal within its 100 ms wait cap, not
    /// at the end of a long `--poll-us` or `--rescan-ms`.
    fn a_sleeping_reader_stops_promptly(detect: &str) {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("node_raw_book_diffs");
        let dir = root.join("hourly").join("20261119");
        fs::create_dir_all(&dir).unwrap();
        // A followed file, so the rescan timer is the long one.
        fs::write(dir.join("8"), "").unwrap();
        let (mut child, mut err, _pre) = spawn_ready(
            bin()
                .arg("node")
                .arg(&root)
                .arg("--out")
                .arg(t.path().join("obs.jsonl"))
                .args(["--detect", detect, "--poll-us", "30000000"])
                .args(["--rescan-ms", "30000"]),
        );
        let _guard = watchdog(&child);
        // Let it reach its first long wait.
        std::thread::sleep(Duration::from_millis(300));
        let start = Instant::now();
        send(&child, "TERM");
        read_until(&mut err, READER_STOPPED);
        let took = start.elapsed();
        let mut rest = String::new();
        err.read_to_string(&mut rest).unwrap();
        let status = child.wait().unwrap();
        assert_eq!(status.code(), Some(0), "{status}: {rest}");
        assert_eq!(counters(rest.as_bytes())["stop_signal"], 15);
        // The cap is 100 ms; the margin is for a loaded machine. Without the
        // cap this is 30 s (or a kill).
        assert!(took < Duration::from_secs(5), "stopped after {took:?}");
    }

    #[test]
    fn a_long_poll_sleep_does_not_delay_the_stop() {
        a_sleeping_reader_stops_promptly("poll");
    }

    #[test]
    fn an_inotify_wait_does_not_delay_the_stop() {
        // Off Linux this falls back to poll, which the other test covers.
        a_sleeping_reader_stops_promptly("inotify");
    }

    /// The way out when the drain cannot finish (here: `--out` is a FIFO
    /// nobody reads any more). The first signal stops the reader and warns;
    /// the second ends the process at once with 128 + the signal number and
    /// no counters line, which marks the run invalid.
    #[test]
    fn a_second_signal_forces_exit_while_the_drain_is_stuck() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("node_raw_book_diffs");
        let dir = root.join("hourly").join("20261119");
        fs::create_dir_all(&dir).unwrap();
        let staged = t.path().join("staged");
        // About 2.8 MB of output: far more than a pipe holds.
        fs::write(&staged, burst(30_000)).unwrap();
        let fifo = t.path().join("obs.fifo");
        assert!(Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success());
        let mut child = bin()
            .arg("node")
            .arg(&root)
            .arg("--out")
            .arg(&fifo)
            .args(["--detect", "poll", "--poll-us", "1000"])
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let _guard = watchdog(&child);
        // The binary's open of the FIFO waits for this reader.
        let mut pipe = BufReader::new(fs::File::open(&fifo).unwrap());
        let mut err = BufReader::new(child.stderr.take().unwrap());
        read_until(&mut err, READY);
        fs::rename(&staged, dir.join("8")).unwrap();
        // Read a little, proving the writer is at work, then stop reading
        // (the FIFO stays open, so the writer blocks instead of failing).
        let mut line = String::new();
        for _ in 0..100 {
            line.clear();
            assert!(pipe.read_line(&mut line).unwrap() > 0);
        }
        // Time for the reader to take in the rest of the 3.5 MB burst.
        std::thread::sleep(Duration::from_millis(1000));
        send(&child, "TERM");
        let before = read_until(&mut err, READER_STOPPED);
        let warning: serde_json::Value = serde_json::from_str(
            before
                .lines()
                .find(|l| l.starts_with("{\"signal\""))
                .unwrap_or_else(|| panic!("no signal line before the stop line: {before}")),
        )
        .unwrap();
        assert_eq!(warning["signal"], 15);
        assert!(
            warning["note"].as_str().unwrap().contains("second signal"),
            "{warning}"
        );
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            child.try_wait().unwrap().is_none(),
            "the writer is stuck on the full FIFO, so the process is still there"
        );
        send(&child, "TERM");
        let status = child.wait().unwrap();
        assert_eq!(status.code(), Some(128 + 15), "{status}");
        let mut rest = String::new();
        err.read_to_string(&mut rest).unwrap();
        assert!(!rest.contains("\"lines\""), "no counters line: {rest}");
        drop(pipe);
    }
}
