//! Timing harness for the capture loop: per-tick cost, write-to-stamp delay
//! per detect mode, and the cost of the directory walk that looks for a
//! newer hourly file, and the writer's drain rate. An `n-detect` of 0 runs
//! only the tick and drain parts.
//!
//! ```text
//! cargo bench -p hl-capture --bench capture -- [n-tick] [n-detect]
//! ```
//!
//! Prints percentiles in microseconds. Write-to-stamp delay runs from just
//! before the writer's `write` call to the reader's stamp, so it includes
//! the `write` itself. Numbers from a shared machine are a sample, not a
//! ranking. `benches/legacy.rs` measures the loop from before this change.
#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use hl_capture::{newest_hourly_file, ring, Detect, NodeTailer, Waiter};
use hl_latency::Observation;

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

/// tmpfs when there is one, so the disk is not what is measured.
fn tmp_root() -> &'static Path {
    let shm = Path::new("/dev/shm");
    if shm.is_dir() {
        shm
    } else {
        Path::new(".")
    }
}

/// One tick: a line is appended (not timed), then one `poll_into` that
/// pushes into the ring is timed. The record is popped untimed.
fn tick(events: usize, n: u64) {
    let t = tempfile::tempdir_in(tmp_root()).unwrap();
    let mut file = hourly_file(t.path());
    let mut tail = NodeTailer::new(t.path());
    let base = Instant::now();
    let mut clock = move || ns(base.elapsed());
    let (mut sink, mut source) = ring(64);
    tail.poll_into(&mut clock, &mut sink).unwrap();
    let mut samples = Vec::with_capacity(usize::try_from(n).unwrap());
    for h in 0..n + 200 {
        file.write_all(&node_line(h, events)).unwrap();
        let start = Instant::now();
        tail.poll_into(&mut clock, &mut sink).unwrap();
        let took = ns(start.elapsed());
        assert_eq!(source.pop().unwrap().block_number, Some(h));
        if h >= 200 {
            samples.push(took);
        }
    }
    assert_eq!(sink.dropped(), 0);
    report(&format!("tick/poll_into+ring/{events}-events"), samples);
}

/// Write-to-stamp delay in one detect mode, with the binary's loop shape:
/// poll, follow, rescan timer, wait.
fn detect(mode: Detect, poll: Duration, n: u64) {
    let t = tempfile::tempdir_in(tmp_root()).unwrap();
    let mut file = hourly_file(t.path());
    let mut tail = NodeTailer::new(t.path());
    let base = Instant::now();
    let mut clock = move || ns(base.elapsed());
    let cpus = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let (mut waiter, note) = Waiter::new(mode, poll, cpus);
    let label = format!("detect/{}/{}us", mode.name(), poll.as_micros());
    if waiter.mode() != mode {
        println!(
            "{label}: not run, fell back to {} ({note:?})",
            waiter.mode().name()
        );
        return;
    }
    let mut out: Vec<Observation> = Vec::with_capacity(usize::try_from(n).unwrap());
    tail.poll_into(&mut clock, &mut out).unwrap();
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
    let rescan = Duration::from_millis(10);
    let mut next_rescan = Instant::now();
    let give_up = next_rescan + Duration::from_secs(60) + Duration::from_millis(2 * n);
    while u64::try_from(out.len()).unwrap() < n {
        let now = Instant::now();
        if now >= give_up {
            break;
        }
        if now >= next_rescan {
            tail.request_rescan();
            next_rescan = now + rescan;
        }
        tail.poll_into(&mut clock, &mut out).unwrap();
        assert_eq!(waiter.follow(t.path(), tail.current_file()), None);
        if !tail.backlogged() && waiter.wait(next_rescan).rescan {
            tail.request_rescan();
        }
    }
    writer.join().unwrap();
    let samples = out
        .iter()
        .map(|o| {
            let h = usize::try_from(o.block_number.unwrap()).unwrap();
            o.recv_ns.saturating_sub(sent[h].load(Ordering::Relaxed))
        })
        .collect();
    report(&label, samples);
}

/// The walk a rescan does, over `dates` date directories of 24 hourly files.
fn walk(dates: u32, n: u64) {
    let t = tempfile::tempdir_in(tmp_root()).unwrap();
    for d in 0..dates {
        let dir = t.path().join("hourly").join(format!("202611{:02}", d + 1));
        fs::create_dir_all(&dir).unwrap();
        for h in 0..24 {
            fs::write(dir.join(h.to_string()), b"").unwrap();
        }
    }
    let mut samples = Vec::with_capacity(usize::try_from(n).unwrap());
    for _ in 0..n {
        let start = Instant::now();
        let newest = newest_hourly_file(t.path()).unwrap();
        samples.push(ns(start.elapsed()));
        assert!(newest.is_some());
    }
    report(&format!("walk/{dates}-dates-x-24-files"), samples);
}

/// The writer's side: how fast it serialises what the ring holds. A file of
/// `n` lines is read into the ring (not timed), then written out as JSON
/// lines to memory (timed). Prints records per second per round.
fn drain(n: u64, rounds: usize) {
    let mut rates = Vec::with_capacity(rounds);
    for _ in 0..rounds {
        let t = tempfile::tempdir_in(tmp_root()).unwrap();
        let mut file = hourly_file(t.path());
        let mut tail = NodeTailer::new(t.path());
        let mut clock = || 1_795_075_200_070_123_456_u64;
        let (mut sink, mut source) = ring(usize::try_from(n).unwrap());
        tail.poll_into(&mut clock, &mut sink).unwrap();
        let body: Vec<u8> = (0..n).flat_map(|h| node_line(h, 2)).collect();
        file.write_all(&body).unwrap();
        while {
            tail.poll_into(&mut clock, &mut sink).unwrap();
            tail.backlogged()
        } {}
        assert_eq!(sink.dropped(), 0);
        let mut out = Vec::with_capacity(usize::try_from(n).unwrap() * 160);
        let start = Instant::now();
        let written = source.write_available(&mut out).unwrap();
        let took = start.elapsed();
        assert_eq!(written, n);
        rates.push(written * 1_000_000_000 / ns(took).max(1));
    }
    rates.sort_unstable();
    println!(
        "drain/write_available/{n}-records   rounds={rounds} min={} median={} max={} records/s",
        rates[0],
        rates[rates.len() / 2],
        rates[rates.len() - 1]
    );
}

fn main() {
    let mut args = std::env::args().skip(1).filter(|a| !a.starts_with("--"));
    let n_tick: u64 = args.next().map_or(20_000, |a| a.parse().unwrap());
    let n_detect: u64 = args.next().map_or(5000, |a| a.parse().unwrap());
    tick(2, n_tick);
    tick(600, n_tick / 10);
    drain(50_000, 21);
    if n_detect == 0 {
        return;
    }
    detect(Detect::BusyPoll, Duration::ZERO, n_detect);
    detect(Detect::Inotify, Duration::ZERO, n_detect);
    detect(Detect::Poll, Duration::from_micros(100), n_detect);
    detect(Detect::Poll, Duration::from_micros(10), n_detect);
    walk(1, 2000);
    walk(30, 2000);
}
