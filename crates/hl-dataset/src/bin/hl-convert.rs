//! `hl-convert`: convert the Hyperliquid Reason's public-data capture to
//! dataset format v2 and print what it saw as JSON.
//!
//! ```text
//! hl-convert --meta meta.json --blocks blocks_core.ndjson|- --l2 l2_anchored.ndjson --out dataset.jsonl [--strict]
//! ```
//!
//! `--blocks -` reads stdin, so the gzip file can be piped through `zcat`.
//! Exit codes: 0 converted; 1 converted but `--strict` and some leg broke the
//! tick/lot rule or failed to parse; 2 usage or conversion error.

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, BufWriter};
use std::path::PathBuf;
use std::process::ExitCode;

use hl_dataset::convert::{assets_from_meta, convert, ConvertStats};

const USAGE: &str = "usage: hl-convert --meta META --blocks BLOCKS|- --l2 L2 --out OUT [--strict]";

#[derive(Default)]
struct Args {
    meta: Option<PathBuf>,
    blocks: Option<PathBuf>,
    l2: Option<PathBuf>,
    out: Option<PathBuf>,
    strict: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = std::env::args_os().skip(1);
    while let Some(arg) = it.next() {
        let slot = match arg.to_str() {
            Some("--meta") => &mut a.meta,
            Some("--blocks") => &mut a.blocks,
            Some("--l2") => &mut a.l2,
            Some("--out") => &mut a.out,
            Some("--strict") => {
                a.strict = true;
                continue;
            }
            _ => return Err(USAGE.to_owned()),
        };
        *slot = Some(PathBuf::from(it.next().ok_or_else(|| USAGE.to_owned())?));
    }
    Ok(a)
}

fn run(a: &Args) -> Result<ConvertStats, String> {
    let need = |p: &Option<PathBuf>| p.clone().ok_or_else(|| USAGE.to_owned());
    let (meta, blocks, l2, out) = (
        need(&a.meta)?,
        need(&a.blocks)?,
        need(&a.l2)?,
        need(&a.out)?,
    );
    let meta_text = fs::read_to_string(&meta).map_err(|e| format!("{}: {e}", meta.display()))?;
    let l2_text = fs::read_to_string(&l2).map_err(|e| format!("{}: {e}", l2.display()))?;
    let assets = assets_from_meta(&meta_text).map_err(|e| e.to_string())?;
    let input: Box<dyn BufRead> = if blocks.as_os_str() == "-" {
        Box::new(io::stdin().lock())
    } else {
        Box::new(BufReader::new(
            File::open(&blocks).map_err(|e| format!("{}: {e}", blocks.display()))?,
        ))
    };
    let out_file = File::create(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let source = format!(
        "hl-convert from Hyperliquid Reason public-data capture: explorer blockDetails ({}), l2Book anchored ({}), meta ({})",
        blocks.display(),
        l2.display(),
        meta.display()
    );
    convert(&assets, &source, input, &l2_text, BufWriter::new(out_file)).map_err(|e| e.to_string())
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(m) => {
            eprintln!("{m}");
            return ExitCode::from(2);
        }
    };
    match run(&args) {
        Ok(stats) => {
            match serde_json::to_string_pretty(&stats) {
                Ok(s) => println!("{s}"),
                Err(e) => eprintln!("could not print stats: {e}"),
            }
            let bad = stats.px_violations + stats.sz_violations + stats.parse_failures;
            if args.strict && bad > 0 {
                eprintln!("--strict: {bad} tick/lot violation(s) or parse failure(s)");
                return ExitCode::from(1);
            }
            ExitCode::SUCCESS
        }
        Err(m) => {
            eprintln!("error: {m}");
            ExitCode::from(2)
        }
    }
}
