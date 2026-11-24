//! Timing harness for issue #31: what the look for a newer hourly file
//! costs the reader, and how long a new file's first line waits.
//!
//! ```text
//! cargo bench -p hl-capture --bench rotation -- [n-blind] [n-files]
//! HL_CAPTURE_BIN=/path/to/another/hl-capture cargo bench ... (second part)
//! ```
//!
//! 1. `blind/...`: how long one `poll_into` at the end of the current file
//!    keeps the reader from reading when a look is due and nothing newer
//!    exists: the reader walking itself (`reader-walk`, the loop before
//!    #31) against the reader taking from the scout's slot (`scout`).
//! 2. `first-line/inotify`: the real binary with `--detect inotify`; from
//!    just before a new hourly file is created and its first line written,
//!    to that line's stamp. `HL_CAPTURE_BIN` runs another build of the
//!    binary through the same harness.
//!
//! Prints percentiles in microseconds. Numbers from a shared machine are a
//! sample, not a ranking.
#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use hl_capture::{scout, NodeTailer};
use hl_latency::Observation;

/// tmpfs when there is one, so the disk is not what is measured.
fn tmp_root() -> &'static Path {
    let shm = Path::new("/dev/shm");
    if shm.is_dir() {
        shm
    } else {
        Path::new(".")
    }
}

fn ns(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap()
}

fn report(arm: &str, mut v: Vec<u64>) {
    v.sort_unstable();
    let at = |per_10k: usize| v[((v.len() - 1) * per_10k) / 10_000];
    let us = |n: u64| format!("{}.{:02}", n / 1000, (n % 1000) / 10);
    println!(
        "{arm:<34} n={:<6} p50={:>9} p90={:>9} p99={:>9} max={:>10} us",
        v.len(),
        us(at(5000)),
        us(at(9000)),
        us(at(9900)),
        us(at(10_000)),
    );
}

fn tree(root: &Path, dates: u32) {
    for d in 0..dates {
        let dir = root.join("hourly").join(format!("202611{:02}", d + 1));
        fs::create_dir_all(&dir).unwrap();
        for h in 0..24 {
            fs::write(dir.join(h.to_string()), b"").unwrap();
        }
    }
}

/// Reader-side cost of a due look that finds nothing newer.
fn blind(dates: u32, n: usize) {
    let mut sink: Vec<Observation> = Vec::new();
    let mut clock = || 1_u64;

    let t = tempfile::tempdir_in(tmp_root()).unwrap();
    tree(t.path(), dates);
    let mut tail = NodeTailer::new(t.path());
    tail.poll_into(&mut clock, &mut sink).unwrap();
    let mut samples = Vec::with_capacity(n);
    for _ in 0..n {
        tail.request_rescan();
        let start = Instant::now();
        tail.poll_into(&mut clock, &mut sink).unwrap();
        samples.push(ns(start.elapsed()));
    }
    report(&format!("blind/reader-walk/{dates}-dates"), samples);

    let (mut sc, arrivals) = scout(t.path(), Duration::from_secs(3600));
    let mut tail = NodeTailer::with_scout(t.path(), 1 << 20, arrivals);
    tail.poll_into(&mut clock, &mut sink).unwrap();
    let mut samples = Vec::with_capacity(n);
    for _ in 0..n {
        tail.request_rescan();
        // The walk itself, off the clock: it runs on the writer thread.
        sc.tick(Instant::now()).unwrap();
        let start = Instant::now();
        std::hint::black_box(tail.look_pending());
        tail.poll_into(&mut clock, &mut sink).unwrap();
        samples.push(ns(start.elapsed()));
    }
    report(&format!("blind/scout/{dates}-dates"), samples);
    assert_eq!(sink.len(), 0, "nothing was written to read");
}

fn realtime_ns() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    )
    .unwrap()
}

fn node_line(height: usize) -> String {
    format!(
        "{{\"local_time\":\"2026-11-19T08:00:00.071123456\",\"block_time\":\"2026-11-19T08:00:00.070000\",\"block_number\":{height},\"events\":[]}}\n"
    )
}

/// New-file first-line delay through the real binary in inotify mode.
fn first_line(n: usize) {
    let bin = std::env::var_os("HL_CAPTURE_BIN")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_hl-capture").into());
    let t = tempfile::tempdir_in(tmp_root()).unwrap();
    let root = t.path().join("node");
    let out = t.path().join("obs.jsonl");
    let path = |i: usize| {
        root.join("hourly")
            .join(format!("202611{:02}", 1 + i / 24))
            .join((i % 24).to_string())
    };
    fs::create_dir_all(path(0).parent().unwrap()).unwrap();
    fs::write(path(0), b"").unwrap();
    let mut child = Command::new(&bin)
        .arg("node")
        .arg(&root)
        .arg("--out")
        .arg(&out)
        .args(["--detect", "inotify"])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut err = BufReader::new(child.stderr.take().unwrap());
    let mut line = String::new();
    while !line.contains("\"ready\"") {
        line.clear();
        assert!(err.read_line(&mut line).unwrap() > 0, "no ready line");
    }
    let drain = std::thread::spawn(move || {
        let mut rest = String::new();
        std::io::Read::read_to_string(&mut err, &mut rest).unwrap();
        rest
    });
    let mut sent = vec![0_u64; n + 1];
    for (i, slot) in sent.iter_mut().enumerate().skip(1) {
        std::thread::sleep(Duration::from_millis(20));
        let p = path(i);
        let text = node_line(i);
        if i % 24 == 0 {
            // A new date: the node makes the directory first.
            fs::create_dir_all(p.parent().unwrap()).unwrap();
        }
        *slot = realtime_ns();
        let mut f = fs::File::create(&p).unwrap();
        f.write_all(text.as_bytes()).unwrap();
    }
    std::thread::sleep(Duration::from_millis(300));
    // SIGTERM: the binary stops cleanly and prints its counters.
    Command::new("kill")
        .arg(child.id().to_string())
        .status()
        .unwrap();
    let status = child.wait().unwrap();
    let rest = drain.join().unwrap();
    let mut samples = Vec::new();
    for l in fs::read_to_string(&out).unwrap().lines() {
        let o: Observation = serde_json::from_str(l).unwrap();
        let h = usize::try_from(o.block_number.unwrap()).unwrap();
        samples.push(o.recv_ns.saturating_sub(sent[h]));
    }
    println!(
        "# {} exit {status}; {}",
        Path::new(&bin).display(),
        rest.lines().last().unwrap_or("")
    );
    assert_eq!(samples.len(), n, "every new file's first line was captured");
    report("first-line/inotify", samples);
}

fn main() {
    let mut args = std::env::args().skip(1).filter(|a| !a.starts_with("--"));
    let n_blind: usize = args.next().map_or(2000, |a| a.parse().unwrap());
    let n_files: usize = args.next().map_or(240, |a| a.parse().unwrap());
    if n_blind > 0 {
        blind(1, n_blind);
        blind(30, n_blind);
    }
    if n_files > 0 {
        first_line(n_files);
    }
}
