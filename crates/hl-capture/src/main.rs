//! `hl-capture`: the probe's capture side.
//!
//! ```text
//! hl-capture node <dir> --out <obs.jsonl> [--poll-us N] [--duration-s N]
//! ```
//!
//! `node` follows the newest hourly file under `<dir>` (for example
//! `~/hl/data/node_raw_book_diffs`) and appends one
//! [`hl_latency::Observation`] per line to `--out`, stamped with
//! `CLOCK_REALTIME` right after each `read`. It polls every `--poll-us`
//! microseconds (default 100: pin the probe to its own core) and stops after
//! `--duration-s` seconds (default: never). Counters go to stderr at exit.
//! Exit codes: 0 ok, 1 I/O error, 2 usage.
//!
//! The public `WebSocket` arm (`hl_capture::WsFilter`) gets its socket
//! client in the next change.

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use hl_capture::NodeTailer;

const USAGE: &str = "usage: hl-capture node <dir> --out <obs.jsonl> [--poll-us N] [--duration-s N]";

struct Args {
    root: PathBuf,
    out: PathBuf,
    poll: Duration,
    duration: Option<Duration>,
}

fn parse_args(mut it: impl Iterator<Item = OsString>) -> Option<Args> {
    if it.next()? != "node" {
        return None;
    }
    let root = PathBuf::from(it.next()?);
    let (mut out, mut poll, mut duration) = (None, Duration::from_micros(100), None);
    while let Some(flag) = it.next() {
        let value = it.next()?;
        let num = || value.to_str()?.parse::<u64>().ok();
        match flag.to_str()? {
            "--out" => out = Some(PathBuf::from(&value)),
            "--poll-us" => poll = Duration::from_micros(num()?),
            "--duration-s" => duration = Some(Duration::from_secs(num()?)),
            _ => return None,
        }
    }
    Some(Args {
        root,
        out: out?,
        poll,
        duration,
    })
}

fn realtime_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_nanos()).ok())
        .unwrap_or(0)
}

fn run(args: &Args) -> std::io::Result<()> {
    let mut out = std::io::BufWriter::new(
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&args.out)?,
    );
    let mut tail = NodeTailer::new(&args.root);
    let mut clock = realtime_ns;
    let start = Instant::now();
    while args.duration.is_none_or(|d| start.elapsed() < d) {
        let obs = tail.poll(&mut clock)?;
        // Serialise after stamping; flush per batch so a crash loses little.
        for o in &obs {
            serde_json::to_writer(&mut out, o)?;
            out.write_all(b"\n")?;
        }
        if !obs.is_empty() {
            out.flush()?;
        }
        std::thread::sleep(args.poll);
    }
    out.flush()?;
    let s = tail.stats();
    eprintln!(
        "{{\"lines\":{},\"bad_lines\":{},\"rotations\":{},\"reads\":{}}}",
        s.lines, s.bad_lines, s.rotations, s.reads
    );
    Ok(())
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
