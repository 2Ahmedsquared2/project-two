//! `book-check`: replay a recorded dataset through a candidate book and report
//! the first divergence.
//!
//! ```text
//! book-check <dataset.jsonl> [--candidate ref]
//! book-check <dataset.jsonl> --replay [--no-stp] [--no-compare]
//! ```
//!
//! `--replay` replays the raw signed actions (`txs`) through the reference
//! book instead of the per-order `ops`, and prints the replay counts.
//! `--no-stp` turns self-trade prevention off; `--no-compare` replays the
//! whole dataset without comparing recorded books (public data has no seed
//! book, so a comparison would diverge at the first snapshot).
//!
//! Exit codes: 0 clean, 1 divergence or rejected op, 2 usage or unreadable dataset.

use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::process::ExitCode;

use book_check::replay::ReplayConfig;
use book_check::{check, check_replay, CheckError};
use hl_book_ref::Book;
use hl_dataset::Reader;

const USAGE: &str =
    "usage: book-check <dataset.jsonl> [--candidate ref] | <dataset.jsonl> --replay [--no-stp] [--no-compare]";

struct Args {
    dataset: PathBuf,
    candidate: String,
    replay: bool,
    stp: bool,
    compare: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut dataset = None;
    let mut candidate = "ref".to_owned();
    let (mut replay, mut stp, mut compare) = (false, true, true);
    let mut it = std::env::args_os().skip(1);
    while let Some(arg) = it.next() {
        match arg.to_str() {
            Some("--candidate") => {
                candidate = it
                    .next()
                    .and_then(|s| s.to_str().map(str::to_owned))
                    .ok_or_else(|| "--candidate needs a value".to_owned())?;
            }
            Some("--replay") => replay = true,
            Some("--no-stp") => stp = false,
            Some("--no-compare") => compare = false,
            Some("-h" | "--help") => return Err(USAGE.to_owned()),
            Some(s) if s.starts_with('-') => return Err(format!("unknown option {s}\n{USAGE}")),
            _ => {
                if dataset.replace(PathBuf::from(arg)).is_some() {
                    return Err(format!("only one dataset may be given\n{USAGE}"));
                }
            }
        }
    }
    let dataset = dataset.ok_or_else(|| USAGE.to_owned())?;
    if !replay && (!stp || !compare) {
        return Err(format!("--no-stp and --no-compare need --replay\n{USAGE}"));
    }
    Ok(Args {
        dataset,
        candidate,
        replay,
        stp,
        compare,
    })
}

fn run(args: &Args) -> Result<book_check::Report, CheckError> {
    let file = File::open(&args.dataset).map_err(hl_dataset::ReadError::from)?;
    let reader = Reader::new(BufReader::new(file))?;
    match args.candidate.as_str() {
        "ref" => check(reader, |_| Book::new()),
        other => Err(CheckError::Read(hl_dataset::ReadError::Io(
            std::io::Error::other(format!("unknown candidate {other:?}; known: ref")),
        ))),
    }
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::from(2);
        }
    };
    if args.replay {
        return run_replay(&args);
    }
    match run(&args) {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(e @ CheckError::Read(_)) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(1)
        }
    }
}

fn run_replay(args: &Args) -> ExitCode {
    let reader = match File::open(&args.dataset)
        .map_err(hl_dataset::ReadError::from)
        .and_then(|f| Reader::new(BufReader::new(f)))
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: dataset: {e}");
            return ExitCode::from(2);
        }
    };
    let config = ReplayConfig {
        self_trade_prevention: args.stp,
        ..ReplayConfig::default()
    };
    match check_replay(reader, config, args.compare) {
        Ok(report) => {
            print!("{report}");
            ExitCode::SUCCESS
        }
        Err(failure) => {
            let code = if matches!(failure.error, CheckError::Read(_)) {
                2
            } else {
                1
            };
            eprintln!("{}", failure.error);
            eprint!("replay counts up to the failure:\n{}", failure.report.stats);
            ExitCode::from(code)
        }
    }
}
