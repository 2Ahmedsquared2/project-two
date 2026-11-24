//! `hl-latency`: summarise same-block arrival times from an observation file.
//!
//! ```text
//! hl-latency <observations.jsonl> [--strict-pass <strict-pass.jsonl>]
//! ```
//!
//! Prints the [`hl_latency::Summary`] as JSON, with the
//! [`hl_latency::StrictDrop`] fields beside it. `--strict-pass` takes the
//! stdout of `hl-strict-pass` over the same hourly files and leaves what it
//! lists as scan-accepted failures out of the pairing: the one observation
//! of each listed line when the observations carry their file and offset
//! (`strict_drop_by_line`), else every observation of each listed block.
//!
//! Exit codes: 0 ok; 1 the summary was printed but the strict pass capped its
//! failure list (`strict_drop_incomplete`), so torn stamps may remain; 2
//! usage, an unreadable or malformed file, or observations and strict-pass
//! output that cannot be matched (some observations with offsets and some
//! without, a listed path with no date and hour, two roots, or a listed line
//! whose observation is another block).

use std::ffi::OsString;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::process::ExitCode;

use hl_latency::{StrictDrop, StrictFailures, StrictReadError, Summary};

const USAGE: &str = "usage: hl-latency <observations.jsonl> [--strict-pass <strict-pass.jsonl>]";

#[derive(serde::Serialize)]
struct Report {
    #[serde(flatten)]
    summary: Summary,
    #[serde(flatten)]
    strict: StrictDrop,
}

/// The observation file and the strict-pass file, when the arguments are one
/// path and at most one `--strict-pass FILE`, in either order.
fn parse_args<I: Iterator<Item = OsString>>(mut args: I) -> Option<(OsString, Option<OsString>)> {
    let (mut obs, mut strict) = (None, None);
    while let Some(arg) = args.next() {
        let slot = if arg == "--strict-pass" {
            (&mut strict, args.next()?)
        } else {
            (&mut obs, arg)
        };
        if slot.0.replace(slot.1).is_some() {
            return None;
        }
    }
    Some((obs?, strict))
}

fn main() -> ExitCode {
    // `args_os`: a non-UTF-8 path must be an error, not a panic.
    let Some((path, strict_path)) = parse_args(std::env::args_os().skip(1)) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let obs = match File::open(&path)
        .map_err(hl_latency::ReadError::from)
        .and_then(|f| hl_latency::read_observations(BufReader::new(f)))
    {
        Ok(obs) => obs,
        Err(e) => {
            eprintln!("{}: {e}", Path::new(&path).display());
            return ExitCode::from(2);
        }
    };
    let (obs, strict) = match strict_path {
        None => (obs, StrictDrop::default()),
        Some(p) => {
            let failures = match File::open(&p)
                .map_err(StrictReadError::from)
                .and_then(|f| StrictFailures::read(BufReader::new(f)))
            {
                Ok(failures) => failures,
                Err(e) => {
                    eprintln!("{}: {e}", Path::new(&p).display());
                    return ExitCode::from(2);
                }
            };
            match failures.drop_from(obs) {
                Ok(dropped) => dropped,
                Err(e) => {
                    eprintln!(
                        "{} against {}: {e}",
                        Path::new(&path).display(),
                        Path::new(&p).display()
                    );
                    return ExitCode::from(2);
                }
            }
        }
    };
    let report = Report {
        summary: hl_latency::summarize(&hl_latency::pair(obs)),
        strict,
    };
    match serde_json::to_string_pretty(&report) {
        Ok(s) => println!("{s}"),
        Err(e) => {
            eprintln!("serialise: {e}");
            return ExitCode::from(2);
        }
    }
    if strict.strict_fallback_lines_unchecked {
        eprintln!(
            "note: hl-strict-pass lists {} failures the scan did not accept, and these observations carry no line_offset, so a stamped one cannot be found and its stamp stays in this summary",
            strict.strict_fallback_failures_listed
        );
    }
    if strict.strict_drop_incomplete {
        eprintln!(
            "warning: hl-strict-pass left {} failures out of its list, so torn stamps may remain in this summary; rerun it with a larger --max-failures",
            strict.strict_failures_omitted
        );
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}
