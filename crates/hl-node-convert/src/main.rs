//! `hl-node-convert`: turn hl-node output into a dataset for `book-check`.
//!
//! ```text
//! hl-node-convert --meta META --diffs FILE [--diffs FILE]... [--statuses FILE]...
//!     [--seed SNAPSHOT] [--check SNAPSHOT]... [--coin COIN]... [--chain NAME]
//!     [--source TEXT] [--no-gaps] [--strict] --out OUT
//! ```
//!
//! Several `--diffs` (or `--statuses`) files are read in the order given, as
//! one stream (pass the node's hourly files oldest first). `META` holds
//! `{"meta": ..., "spotMeta": ...}` as for `hl-convert`. The stats are printed
//! to stdout as JSON.
//!
//! Exit codes: 0 converted; 1 a `--strict` finding; 2 usage or conversion error.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use hl_dataset::convert::assets_from_meta;
use hl_node_convert::convert::{convert, ConvertError, Options, Stats};
use hl_node_convert::snapshot::L4Snapshot;

const USAGE: &str = "usage: hl-node-convert --meta META --diffs FILE [--diffs FILE]... \
[--statuses FILE]... [--seed SNAPSHOT] [--check SNAPSHOT]... [--coin COIN]... \
[--chain NAME] [--source TEXT] [--no-gaps] [--strict] --out OUT";

const DEFAULT_SOURCE: &str = "hl-node node_raw_book_diffs + node_order_statuses";

#[derive(Default)]
struct Args {
    meta: Option<PathBuf>,
    diffs: Vec<PathBuf>,
    statuses: Vec<PathBuf>,
    seed: Option<PathBuf>,
    checks: Vec<PathBuf>,
    coins: BTreeSet<String>,
    chain: Option<String>,
    source: Option<String>,
    out: Option<PathBuf>,
    no_gaps: bool,
    strict: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = std::env::args_os().skip(1);
    while let Some(arg) = it.next() {
        let flag = arg.to_str().unwrap_or_default().to_owned();
        match flag.as_str() {
            "--no-gaps" => {
                a.no_gaps = true;
                continue;
            }
            "--strict" => {
                a.strict = true;
                continue;
            }
            "--meta" | "--diffs" | "--statuses" | "--seed" | "--check" | "--coin" | "--chain"
            | "--source" | "--out" => {}
            _ => {
                return Err(format!(
                    "unknown argument {}\n{USAGE}",
                    arg.to_string_lossy()
                ))
            }
        }
        let value = it
            .next()
            .ok_or_else(|| format!("{flag} needs a value\n{USAGE}"))?;
        let text = || {
            value
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{flag} value is not UTF-8"))
        };
        match flag.as_str() {
            "--meta" => a.meta = Some(value.into()),
            "--diffs" => a.diffs.push(value.into()),
            "--statuses" => a.statuses.push(value.into()),
            "--seed" => a.seed = Some(value.into()),
            "--check" => a.checks.push(value.into()),
            "--coin" => {
                a.coins.insert(text()?);
            }
            "--chain" => a.chain = Some(text()?),
            "--source" => a.source = Some(text()?),
            _ => a.out = Some(value.into()),
        }
    }
    if a.meta.is_none() || a.diffs.is_empty() || a.out.is_none() {
        return Err(USAGE.to_owned());
    }
    Ok(a)
}

/// The files as one stream, a newline after each so a file without a final
/// newline never glues its last line to the next file's first.
fn concat(paths: &[PathBuf]) -> Result<BufReader<Box<dyn Read>>, String> {
    let mut stream: Box<dyn Read> = Box::new(io::empty());
    for p in paths {
        let f = File::open(p).map_err(|e| format!("{}: {e}", p.display()))?;
        stream = Box::new(stream.chain(f).chain(Cursor::new(b"\n")));
    }
    Ok(BufReader::new(stream))
}

fn snapshot(p: &PathBuf) -> Result<L4Snapshot, String> {
    let text = fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
    L4Snapshot::from_json(&text).map_err(|e| format!("{}: {e}", p.display()))
}

enum Failure {
    Strict(String),
    Other(String),
}

/// Reject aliases by file identity (including hardlinks and symlinks), not spelling.
fn protect_inputs(a: &Args, out: &Path) -> Result<(), String> {
    match fs::metadata(out) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("{}: {e}", out.display())),
    }
    for input in a
        .meta
        .iter()
        .chain(&a.diffs)
        .chain(&a.statuses)
        .chain(&a.seed)
        .chain(&a.checks)
    {
        if same_file::is_same_file(input, out).map_err(|e| format!("{}: {e}", input.display()))? {
            return Err(format!(
                "output {} aliases input {}",
                out.display(),
                input.display()
            ));
        }
    }
    Ok(())
}

fn run(a: &Args) -> Result<Stats, Failure> {
    let other = Failure::Other;
    let meta = a.meta.as_ref().ok_or_else(|| other(USAGE.to_owned()))?;
    let out = a.out.as_ref().ok_or_else(|| other(USAGE.to_owned()))?;
    protect_inputs(a, out).map_err(other)?;
    let meta_text =
        fs::read_to_string(meta).map_err(|e| other(format!("{}: {e}", meta.display())))?;
    let assets = assets_from_meta(&meta_text).map_err(|e| other(e.to_string()))?;
    let seed = a.seed.as_ref().map(snapshot).transpose().map_err(other)?;
    let checks = a
        .checks
        .iter()
        .map(snapshot)
        .collect::<Result<Vec<_>, _>>()
        .map_err(other)?;
    let diffs = concat(&a.diffs).map_err(other)?;
    let statuses = if a.statuses.is_empty() {
        None
    } else {
        Some(concat(&a.statuses).map_err(other)?)
    };
    let opts = Options {
        chain: a.chain.clone().unwrap_or_else(|| "mainnet".to_owned()),
        source: a
            .source
            .clone()
            .unwrap_or_else(|| DEFAULT_SOURCE.to_owned()),
        coins: (!a.coins.is_empty()).then(|| a.coins.clone()),
        allow_gaps: !a.no_gaps,
        strict: a.strict,
    };
    // A sibling temporary file keeps publication on one filesystem. Drop removes
    // incomplete output on every error; persist atomically replaces unrelated output.
    let parent = out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent)
        .map_err(|e| other(format!("{}: {e}", out.display())))?;
    let stats = convert(
        &assets,
        diffs,
        statuses,
        seed.as_ref(),
        &checks,
        &opts,
        BufWriter::new(file.as_file_mut()),
    )
    .map_err(|e| match e {
        ConvertError::Strict(m) => Failure::Strict(m),
        e => other(e.to_string()),
    })?;
    file.as_file()
        .sync_all()
        .map_err(|e| other(format!("{}: {e}", out.display())))?;
    protect_inputs(a, out).map_err(other)?;
    file.persist(out)
        .map_err(|e| other(format!("{}: {e}", out.display())))?;
    Ok(stats)
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
        Ok(stats) => match serde_json::to_string_pretty(&stats) {
            Ok(json) => {
                println!("{json}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("stats: {e}");
                ExitCode::from(2)
            }
        },
        Err(Failure::Strict(m)) => {
            eprintln!("strict: {m}");
            ExitCode::from(1)
        }
        Err(Failure::Other(m)) => {
            eprintln!("error: {m}");
            ExitCode::from(2)
        }
    }
}
