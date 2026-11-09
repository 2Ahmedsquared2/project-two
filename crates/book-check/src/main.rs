//! `book-check`: replay a recorded dataset through a candidate book and report
//! the first divergence.
//!
//! ```text
//! book-check <dataset.jsonl> [--candidate ref]
//! ```
//!
//! Exit codes: 0 clean, 1 divergence or rejected op, 2 usage or unreadable dataset.

use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use std::process::ExitCode;

use book_check::{check, CheckError};
use hl_book_ref::Book;
use hl_dataset::Reader;

const USAGE: &str = "usage: book-check <dataset.jsonl> [--candidate ref]";

struct Args {
    dataset: PathBuf,
    candidate: String,
}

fn parse_args() -> Result<Args, String> {
    let mut dataset = None;
    let mut candidate = "ref".to_owned();
    let mut it = std::env::args_os().skip(1);
    while let Some(arg) = it.next() {
        match arg.to_str() {
            Some("--candidate") => {
                candidate = it
                    .next()
                    .and_then(|s| s.to_str().map(str::to_owned))
                    .ok_or_else(|| "--candidate needs a value".to_owned())?;
            }
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
    Ok(Args { dataset, candidate })
}

fn run(args: &Args) -> Result<book_check::Report, CheckError> {
    let file = File::open(&args.dataset).map_err(hl_dataset::ReadError::from)?;
    let reader = Reader::new(BufReader::new(file))?;
    match args.candidate.as_str() {
        "ref" => check(reader, &mut Book::new()),
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
