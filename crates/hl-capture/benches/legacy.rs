//! Timing harness for the capture loop as it was before the detect/ring
//! change: `NodeTailer::poll` returning a `Vec`, and a fixed 100 us sleep.
//!
//! It uses only the API that exists at main `3a197dc`, so copying this file
//! (and its `[[bench]]` entry) into a checkout of that commit measures the
//! old code; run here it measures the compatibility wrapper `poll`.
//!
//! ```text
//! cargo bench -p hl-capture --bench legacy -- [n-tick] [n-detect]
//! ```
//!
//! Prints percentiles in microseconds. Numbers from a shared machine are a
//! sample, not a ranking.
#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use hl_capture::NodeTailer;

fn node_line(height: u64, events: usize) -> Vec<u8> {
    let event = r#"{"user":"0x31ca8395cf837de08b24da3f660e77761dfb974b","oid":90000000001,"coin":"BTC","side":"B","px":"104000.0","raw_book_diff":{"new":{"sz":"0.25"}}}"#;
    let body = vec![event; events].join(",");
    format!(
        "{{\"local_time\":\"2026-11-19T08:00:00.071123456\",\"block_time\":\"2026-11-19T08:00:00.070000\",\"block_number\":{height},\"events\":[{body}]}}\n"
    )
    .into_bytes()
}

fn hourly_file(root: &Path) -> File {
    let dir = root.join("hourly").join("20261119");
    fs::create_dir_all(&dir).unwrap();
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("8"))
        .unwrap()
}

fn ns(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap()
}

fn report(arm: &str, mut v: Vec<u64>) {
    v.sort_unstable();
    let at = |per_10k: usize| v[((v.len() - 1) * per_10k) / 10_000];
    let us = |n: u64| format!("{}.{:02}", n / 1000, (n % 1000) / 10);
    println!(
        "{arm:<34} n={:<6} p50={:>8} p90={:>8} p99={:>8} p99.9={:>9} max={:>9} us",
        v.len(),
        us(at(5000)),
        us(at(9000)),
        us(at(9900)),
        us(at(9990)),
        us(at(10_000)),
    );
}

/// One tick: a line is appended (not timed), then one `poll` is timed.
fn tick(events: usize, n: u64) {
    let t = tempfile::tempdir_in(tmp_root()).unwrap();
    let mut file = hourly_file(t.path());
    let mut tail = NodeTailer::new(t.path());
    let base = Instant::now();
    let mut clock = move || ns(base.elapsed());
    tail.poll(&mut clock).unwrap();
    let mut samples = Vec::with_capacity(usize::try_from(n).unwrap());
    for h in 0..n + 200 {
        file.write_all(&node_line(h, events)).unwrap();
        let start = Instant::now();
        let got = tail.poll(&mut clock).unwrap();
        let took = ns(start.elapsed());
        assert_eq!(got.len(), 1);
        if h >= 200 {
            samples.push(took);
        }
    }
    report(&format!("tick/legacy-poll/{events}-events"), samples);
}

/// Write-to-stamp delay of the old loop: `poll`, then sleep 100 us.
fn detect(n: u64) {
    let t = tempfile::tempdir_in(tmp_root()).unwrap();
    let mut file = hourly_file(t.path());
    let mut tail = NodeTailer::new(t.path());
    let base = Instant::now();
    let mut clock = move || ns(base.elapsed());
    tail.poll(&mut clock).unwrap();
    let sent: Arc<Vec<AtomicU64>> = Arc::new((0..n).map(|_| AtomicU64::new(0)).collect());
    let writer = {
        let sent = Arc::clone(&sent);
        std::thread::spawn(move || {
            for (h, slot) in sent.iter().enumerate() {
                std::thread::sleep(Duration::from_millis(1));
                let line = node_line(u64::try_from(h).unwrap(), 2);
                slot.store(ns(base.elapsed()), Ordering::Relaxed);
                file.write_all(&line).unwrap();
            }
        })
    };
    let mut samples = Vec::with_capacity(usize::try_from(n).unwrap());
    let give_up = Instant::now() + Duration::from_secs(60) + Duration::from_millis(2 * n);
    while u64::try_from(samples.len()).unwrap() < n && Instant::now() < give_up {
        for o in tail.poll(&mut clock).unwrap() {
            let h = usize::try_from(o.block_number.unwrap()).unwrap();
            samples.push(o.recv_ns.saturating_sub(sent[h].load(Ordering::Relaxed)));
        }
        std::thread::sleep(Duration::from_micros(100));
    }
    writer.join().unwrap();
    report("detect/legacy-poll-100us-sleep", samples);
}

/// tmpfs when there is one, so the disk is not what is measured.
fn tmp_root() -> &'static Path {
    let shm = Path::new("/dev/shm");
    if shm.is_dir() {
        shm
    } else {
        Path::new(".")
    }
}

fn main() {
    let mut args = std::env::args().skip(1).filter(|a| !a.starts_with("--"));
    let n_tick: u64 = args.next().map_or(20_000, |a| a.parse().unwrap());
    let n_detect: u64 = args.next().map_or(5000, |a| a.parse().unwrap());
    tick(2, n_tick);
    tick(600, n_tick / 10);
    detect(n_detect);
}
