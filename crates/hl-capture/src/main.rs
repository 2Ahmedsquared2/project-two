//! `hl-capture`: the probe's capture side.
//!
//! ```text
//! hl-capture node <dir> --out <obs.jsonl> [--detect busy-poll|poll|inotify]
//!     [--poll-us N] [--duration-s N] [--max-line-bytes N] [--ring-records N]
//!     [--rescan-ms N]
//! ```
//!
//! `node` follows the newest hourly file under `<dir>` (for example
//! `~/hl/data/node_raw_book_diffs`) and appends one
//! [`hl_latency::Observation`] per line to `--out`, stamped with
//! `CLOCK_REALTIME` right after each `read`.
//!
//! Two threads. The reader reads, stamps, scans each line's header and
//! pushes a fixed-size record into a bounded ring; it does not allocate,
//! serialise or touch the disk. The writer thread serialises and writes. If
//! the ring (`--ring-records`, default 65536) is full the newest record is
//! dropped and counted in `ring_dropped`; the reader is never held up. A run
//! with `ring_dropped` above 0 is incomplete: discard it.
//!
//! `--detect` says how the reader waits for the node to write:
//!
//! - `busy-poll` (default): it does not wait; it spins on `read`. Lowest
//!   delay, one core at 100%: give it a core of its own (`taskset`). With
//!   only one usable CPU it falls back to `poll`.
//! - `poll`: it sleeps `--poll-us` microseconds (default 100) between reads.
//! - `inotify`: it sleeps until the kernel reports a write. Linux only; off
//!   Linux, or if inotify cannot be set up, it falls back to `poll`.
//!
//! The mode asked for, the mode in use and the reason for any fallback are
//! printed on stderr as a `detect` line at start (and again if a later
//! inotify failure forces `poll`).
//!
//! The run stops after `--duration-s` seconds (default: never; a sleep is
//! cut short so the run ends on time). A newer hourly file is looked for every
//! `--rescan-ms` milliseconds (default 10) and, in `inotify` mode, as soon as
//! a directory changes; never per poll. What the switching
//! poll reads from a new file may be stamped up to that much late, so it is
//! reported on stderr as a `rotation` line and counted in
//! `post_rotation_lines`. Lines longer than `--max-line-bytes` (default
//! 1 MiB) are dropped and counted. Counters go to stderr at exit.
//! Exit codes: 0 ok, 1 I/O error, 2 usage.
//!
//! The public `WebSocket` arm (`hl_capture::WsFilter`) gets its socket
//! client in the next change.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use hl_capture::{ring, Detect, NodeTailer, Waiter};

const USAGE: &str = "usage: hl-capture node <dir> --out <obs.jsonl> [--detect busy-poll|poll|inotify] [--poll-us N] [--duration-s N] [--max-line-bytes N] [--ring-records N] [--rescan-ms N]";

/// How long the writer thread sleeps when the ring is empty.
const WRITER_IDLE: Duration = Duration::from_millis(1);

struct Args {
    root: PathBuf,
    out: PathBuf,
    poll: Duration,
    duration: Option<Duration>,
    max_line: usize,
    ring_records: usize,
    rescan: Duration,
    detect: Detect,
}

fn parse_args(mut it: impl Iterator<Item = OsString>) -> Option<Args> {
    if it.next()? != "node" {
        return None;
    }
    let root = PathBuf::from(it.next()?);
    let (mut out, mut poll, mut duration) = (None, Duration::from_micros(100), None);
    let mut max_line = hl_capture::DEFAULT_MAX_LINE;
    let mut ring_records = hl_capture::DEFAULT_RING_RECORDS;
    let mut rescan = Duration::from_millis(10);
    let mut detect = Detect::BusyPoll;
    while let Some(flag) = it.next() {
        let value = it.next()?;
        let num = || value.to_str()?.parse::<u64>().ok();
        let positive = || usize::try_from(num()?).ok().filter(|&n| n > 0);
        match flag.to_str()? {
            "--out" => out = Some(PathBuf::from(&value)),
            "--poll-us" => poll = Duration::from_micros(num()?),
            "--duration-s" => duration = Some(Duration::from_secs(num()?)),
            "--max-line-bytes" => max_line = positive()?,
            "--ring-records" => ring_records = positive()?,
            "--detect" => detect = Detect::parse(value.to_str()?)?,
            "--rescan-ms" => rescan = Duration::from_millis(num().filter(|&n| n > 0)?),
            _ => return None,
        }
    }
    Some(Args {
        root,
        out: out?,
        poll,
        duration,
        max_line,
        ring_records,
        rescan,
        detect,
    })
}

fn realtime_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_nanos()).ok())
        .unwrap_or(0)
}

/// Says on stderr which detect mode is live, and why if it is not the one
/// asked for.
fn announce(requested: Detect, live: Detect, fallback: Option<&str>) {
    let fallback = fallback.map_or_else(
        || "null".to_owned(),
        |why| serde_json::Value::from(why).to_string(),
    );
    eprintln!(
        "{{\"detect\":{{\"requested\":\"{}\",\"live\":\"{}\",\"fallback\":{fallback}}}}}",
        requested.name(),
        live.name()
    );
}

fn run(args: &Args) -> std::io::Result<()> {
    let cpus = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let (mut waiter, note) = Waiter::new(args.detect, args.poll, cpus);
    announce(args.detect, waiter.mode(), note.as_deref());
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&args.out)?;
    let (mut sink, source) = ring(args.ring_records);
    let writer = std::thread::Builder::new()
        .name("hl-capture-writer".into())
        .spawn(move || source.run(std::io::BufWriter::new(file), WRITER_IDLE))?;
    let mut tail = NodeTailer::with_max_line(&args.root, args.max_line);
    let mut clock = realtime_ns;
    let mut next_rescan = Instant::now();
    let deadline = args.duration.and_then(|d| next_rescan.checked_add(d));
    let read = loop {
        let now = Instant::now();
        if deadline.is_some_and(|d| now >= d) {
            break Ok(());
        }
        if now >= next_rescan {
            // Coarse timer: the directory walk happens at most this often,
            // and only once the current file is at its end.
            tail.request_rescan();
            next_rescan = now.checked_add(args.rescan).unwrap_or(now);
        }
        if let Err(e) = tail.poll_into(&mut clock, &mut sink) {
            break Err(e);
        }
        if sink.abandoned() {
            // The writer thread stopped; its error is reported below.
            break Ok(());
        }
        if let Some(why) = waiter.follow(&args.root, tail.current_file()) {
            announce(args.detect, waiter.mode(), Some(&why));
        }
        if let Some(sw) = tail.take_switch() {
            // Cold: once per new file.
            let h = |n: Option<u64>| n.map_or_else(|| "null".into(), |n| n.to_string());
            eprintln!(
                "{{\"rotation\":{{\"late_lines\":{},\"first_block_number\":{},\"last_block_number\":{}}}}}",
                sw.lines,
                h(sw.first_block_number),
                h(sw.last_block_number)
            );
        }
        if tail.backlogged() {
            // More data waits: check the deadline, then read on at once.
            continue;
        }
        // Never wait past the deadline or the next rescan.
        let until = deadline.map_or(next_rescan, |d| d.min(next_rescan));
        if waiter.wait(until).rescan {
            tail.request_rescan();
        }
    };
    let dropped = sink.dropped();
    // Dropping the reader's end tells the writer to drain and stop.
    drop(sink);
    let written = writer
        .join()
        .unwrap_or_else(|_| Err(std::io::Error::other("writer thread panicked")));
    let s = tail.stats();
    eprintln!(
        "{{\"lines\":{},\"written\":{},\"ring_dropped\":{},\"bad_lines\":{},\"oversized_lines\":{},\"scan_fallbacks\":{},\"height_breaks\":{},\"rotations\":{},\"post_rotation_lines\":{},\"rescans\":{},\"reads\":{}}}",
        s.lines,
        written.as_ref().map_or(0, |w| *w),
        dropped,
        s.bad_lines,
        s.oversized_lines,
        s.scan_fallbacks,
        s.height_breaks,
        s.rotations,
        s.post_rotation_lines,
        s.rescans,
        s.reads
    );
    read?;
    written.map(drop)
}

fn main() -> ExitCode {
    let Some(args) = parse_args(std::env::args_os().skip(1)) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("hl-capture: {e}");
            ExitCode::from(1)
        }
    }
}
