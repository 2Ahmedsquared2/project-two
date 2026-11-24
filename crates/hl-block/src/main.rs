//! `hl-block census [--json] <file|dir|->...`: action census of `replica_cmds`
//! files. Directories are walked recursively in numeric-then-name order.
//! Exit 0 clean, 1 if any line failed to decode (counted in the output), 2 usage
//! or I/O error. Compressed files are not read: pipe them (`lz4 -dc f | hl-block census -`).

use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use hl_block::census::Census;
use hl_block::Reader;

const USAGE: &str = "usage: hl-block census [--json] <file|dir|->...";

fn walk(p: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if p.is_dir() {
        let mut kids: Vec<PathBuf> = std::fs::read_dir(p)?
            .map(|e| e.map(|e| e.path()))
            .collect::<Result<_, _>>()?;
        kids.sort_by_key(|k| {
            let n = k
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            (n.parse::<u64>().unwrap_or(u64::MAX), n)
        });
        for k in kids {
            walk(&k, out)?;
        }
    } else {
        out.push(p.to_path_buf());
    }
    Ok(())
}

fn feed<R: std::io::BufRead>(c: &mut Census, r: R, name: &str) {
    for item in Reader::new(r) {
        match item {
            Ok(b) => c.add(&b),
            Err(e) => {
                eprintln!("{name}: {e}");
                c.add_error(&e.error.to_string());
            }
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("census") {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    let json = args.iter().any(|a| a == "--json");
    let inputs: Vec<&String> = args[1..].iter().filter(|a| *a != "--json").collect();
    if inputs.is_empty() {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    let mut c = Census::default();
    for i in inputs {
        if i == "-" {
            feed(&mut c, std::io::stdin().lock(), "-");
            continue;
        }
        let mut files = Vec::new();
        if let Err(e) = walk(Path::new(i), &mut files) {
            eprintln!("{i}: {e}");
            return ExitCode::from(2);
        }
        for f in files {
            match std::fs::File::open(&f) {
                Ok(fh) => feed(
                    &mut c,
                    BufReader::with_capacity(1 << 20, fh),
                    &f.display().to_string(),
                ),
                Err(e) => {
                    eprintln!("{}: {e}", f.display());
                    return ExitCode::from(2);
                }
            }
        }
    }
    let out = if json {
        c.to_json() + "\n"
    } else {
        c.to_text()
    };
    if std::io::stdout().write_all(out.as_bytes()).is_err() {
        return ExitCode::from(2);
    }
    if c.decode_errors.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
