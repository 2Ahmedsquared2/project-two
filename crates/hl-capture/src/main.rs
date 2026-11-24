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
//! Reading stops after `--duration-s` seconds (default: never; a sleep is
//! cut short so it ends on time) and `{"reader_stopped":true}` goes to
//! stderr; the writer then writes what the ring still holds, so the process
//! exits later by the time that backlog takes (nothing read is discarded).
//!
//! `SIGTERM` or `SIGINT` ends a run the same way as the deadline: the reader
//! stops (at once in `busy-poll` and default `poll`; within 100 ms when it
//! sleeps longer, since every wait is capped at that), the writer writes
//! what the ring holds, the counters are printed with `stop_signal` set to
//! the signal's number, and the exit code is 0. Bytes the node wrote that
//! the reader had not read yet are not captured. A second signal ends the
//! process at once with status 128 + the signal number and no counters:
//! that run is invalid; it is the way out when the writer cannot finish.
//! `SIGKILL` and `SIGHUP` are not handled.
//!
//! A newer hourly file is looked for every
//! `--rescan-ms` milliseconds (default 1000) and, in `inotify` mode, as soon
//! as a directory changes; never per poll. The look is a directory walk on
//! the reader thread, which cannot read while it walks (measured 13 us with
//! one date directory, 262 us with thirty, on a shared machine), so a line
//! that lands during a walk is stamped that much late without any flag: keep
//! the interval coarse and prune old date directories. While no file is
//! followed yet there is nothing to read, so the look happens every 10 ms at
//! most. When a newer file is found, the old one is read to its end once
//! more before the switch, so a line appended to it during the look is not
//! lost. What the switching
//! poll reads from a new file may be stamped up to that much late, so it is
//! reported on stderr as a `rotation` line and counted in
//! `post_rotation_lines`. Lines longer than `--max-line-bytes` (default
//! 1 MiB) are dropped and counted. After its first poll it writes
//! `{"ready":true}` to stderr: a file that appears from then on is read from
//! its start (one already there at the first poll is history and is read from
//! its end), and in `inotify` mode the watches are in place. Counters go to
//! stderr at exit.
//! Exit codes: 0 ok, 1 I/O error, 2 usage.
//!
//! The public `WebSocket` arm (`hl_capture::WsFilter`) gets its socket
//! client in the next change.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use hl_capture::{ring, Detect, NodeTailer, Stop, Waiter};

const USAGE: &str = "usage: hl-capture node <dir> --out <obs.jsonl> [--detect busy-poll|poll|inotify] [--poll-us N] [--duration-s N] [--max-line-bytes N] [--ring-records N] [--rescan-ms N]";

/// Longest gap between looks for a first file, while none is followed: with
/// nothing to read, the walk costs no stamp accuracy.
const RESCAN_NO_FILE: Duration = Duration::from_millis(10);

/// Longest single wait of the reader, so a sleeping reader (a long
/// `--poll-us`, or `inotify` with nothing written) notices a stop signal
/// within this long. Costs an idle reader about ten empty reads a second.
const STOP_CHECK: Duration = Duration::from_millis(100);

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
    let mut rescan = Duration::from_millis(1000);
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
    // First, so a signal at any later point stops the run cleanly.
    let stop = Stop::install()?;
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
    let mut ready = false;
    let mut next_rescan = Instant::now();
    let deadline = args.duration.and_then(|d| next_rescan.checked_add(d));
    let read = loop {
        let now = Instant::now();
        // The stop flag is read here, after everything the previous
        // iteration read was stamped and pushed: stopping loses no record.
        if deadline.is_some_and(|d| now >= d) || stop.requested() {
            break Ok(());
        }
        if now >= next_rescan {
            // Coarse timer: the directory walk happens at most this often,
            // and only once the current file is at its end.
            tail.request_rescan();
            let every = if tail.current_file().is_some() {
                args.rescan
            } else {
                args.rescan.min(RESCAN_NO_FILE)
            };
            next_rescan = now.checked_add(every).unwrap_or(now);
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
        if !ready {
            // Cold: once. Callers wait for this before creating files they
            // expect read; it follows the first poll and the first watch.
            eprintln!("{{\"ready\":true}}");
            ready = true;
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
        // Never wait past the deadline or the next rescan, nor longer than
        // a stop signal may go unnoticed.
        let until = deadline.map_or(next_rescan, |d| d.min(next_rescan));
        let until = now.checked_add(STOP_CHECK).map_or(until, |s| s.min(until));
        if waiter.wait(until).rescan {
            tail.request_rescan();
        }
    };
    let dropped = sink.dropped();
    let signal = stop.signal();
    if let Some(n) = signal {
        // Cold: once.
        eprintln!(
            "{{\"signal\":{n},\"note\":\"stopped reading; writing what the ring holds, then the counters. A second signal exits at once without them and makes this run invalid\"}}"
        );
    }
    // Cold: once. Reading ends here, on time; the writer still has whatever
    // the ring holds to write, and that takes as long as the backlog needs.
    eprintln!("{{\"reader_stopped\":true}}");
    // Dropping the reader's end tells the writer to drain and stop.
    drop(sink);
    let written = writer
        .join()
        .unwrap_or_else(|_| Err(std::io::Error::other("writer thread panicked")));
    let s = tail.stats();
    eprintln!(
        "{{\"lines\":{},\"written\":{},\"ring_dropped\":{},\"bad_lines\":{},\"oversized_lines\":{},\"scan_fallbacks\":{},\"height_breaks\":{},\"rotations\":{},\"post_rotation_lines\":{},\"rescans\":{},\"reads\":{},\"stop_signal\":{}}}",
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
        s.reads,
        signal.map_or_else(|| "null".to_owned(), |n| n.to_string())
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
