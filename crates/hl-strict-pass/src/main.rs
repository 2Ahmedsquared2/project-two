//! `hl-strict-pass`: the offline check of the node's hourly output files.
//!
//! ```text
//! hl-strict-pass [--max-failures N] [--max-line-bytes N]
//!     [--allow-unterminated-tail] [--fail-on-height-gap] <path>...
//! ```
//!
//! Each `<path>` is an hourly file, checked as given, or a directory, whose
//! hourly files (`.../{YYYYMMDD}/{H}`) are checked oldest first. Paths are
//! taken in the order given, and heights carry from one file to the next,
//! so name them oldest first.
//!
//! Every line goes through [`hl_strict_pass::check_record`]. stdout is JSON
//! lines: a `failure` line per failed record (at most `--max-failures`,
//! default 100; the counts are never capped), a `file` line per file and one
//! `total` line. A failure the hot path's scan accepts (`scan_accepted`)
//! carries the `block_number` and `block_time_ms` hl-capture recorded for
//! it, so that observation can be dropped from a pairing.
//!
//! `--allow-unterminated-tail` lets the last file checked end without a
//! newline (the hour the node is still writing); that line is not checked.
//! `--fail-on-height-gap` makes a height that skips forward or goes back a
//! failure of the run; a repeated height never is, because with
//! `--stream-with-block-info` one block can span several lines.
//!
//! Exit codes: 0 every record passed; 1 a record failed (or a height gap,
//! when asked); 2 usage; 3 a path could not be read, or there was no hourly
//! file to check.

use std::ffi::OsString;
use std::fs::File;
use std::io::{BufReader, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;

use hl_strict_pass::{hourly_files, Counts, Failure, FileReport, Options, StrictPass};
use serde::Serialize;

const USAGE: &str = "usage: hl-strict-pass [--max-failures N] [--max-line-bytes N] [--allow-unterminated-tail] [--fail-on-height-gap] <path>...";

struct Args {
    opts: Options,
    allow_tail: bool,
    fail_on_gap: bool,
    paths: Vec<PathBuf>,
}

fn parse_args(mut it: impl Iterator<Item = OsString>) -> Option<Args> {
    let mut args = Args {
        opts: Options::default(),
        allow_tail: false,
        fail_on_gap: false,
        paths: Vec::new(),
    };
    while let Some(arg) = it.next() {
        let mut num = || it.next()?.to_str()?.parse::<usize>().ok();
        match arg.to_str() {
            Some("--max-failures") => args.opts.max_failures = num()?,
            Some("--max-line-bytes") => args.opts.max_line = num().filter(|&n| n > 0)?,
            Some("--allow-unterminated-tail") => args.allow_tail = true,
            Some("--fail-on-height-gap") => args.fail_on_gap = true,
            Some(flag) if flag.starts_with("--") => return None,
            _ => args.paths.push(PathBuf::from(arg)),
        }
    }
    (!args.paths.is_empty()).then_some(args)
}

#[derive(Serialize)]
struct Total {
    files: usize,
    #[serde(flatten)]
    counts: Counts,
    failures_omitted: u64,
}

/// One stdout line: `{"failure":{..}}`, `{"file":{..}}` or `{"total":{..}}`.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Line<'a> {
    Failure(&'a Failure),
    File(&'a FileReport),
    Total(Total),
}

fn run(args: &Args) -> std::io::Result<Counts> {
    let mut files = Vec::new();
    for path in &args.paths {
        if std::fs::metadata(path)?.is_dir() {
            files.extend(hourly_files(path)?);
        } else {
            files.push(path.clone());
        }
    }
    if files.is_empty() {
        return Err(std::io::Error::other("no hourly file to check"));
    }
    let mut out = std::io::stdout().lock();
    let mut emit = |line: &Line<'_>| -> std::io::Result<()> {
        serde_json::to_writer(&mut out, line)?;
        out.write_all(b"\n")
    };
    let mut pass = StrictPass::new(args.opts);
    let mut listed = 0;
    for (i, path) in files.iter().enumerate() {
        let newest = i + 1 == files.len();
        let name = path.display().to_string();
        let reader = BufReader::with_capacity(1 << 20, File::open(path)?);
        let report = pass.check_file(&name, reader, args.allow_tail && newest)?;
        for failure in pass.failures().get(listed..).unwrap_or_default() {
            emit(&Line::Failure(failure))?;
        }
        listed = pass.failures().len();
        emit(&Line::File(&report))?;
    }
    emit(&Line::Total(Total {
        files: files.len(),
        counts: pass.total(),
        failures_omitted: pass.failures_omitted(),
    }))?;
    Ok(pass.total())
}

fn main() -> ExitCode {
    let Some(args) = parse_args(std::env::args_os().skip(1)) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    match run(&args) {
        Ok(c) => {
            let gap = args.fail_on_gap && c.height_gaps + c.height_backwards > 0;
            ExitCode::from(u8::from(c.failed > 0 || gap))
        }
        Err(e) => {
            eprintln!("hl-strict-pass: {e}");
            ExitCode::from(3)
        }
    }
}
