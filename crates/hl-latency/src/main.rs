//! `hl-latency`: summarise same-block arrival times from an observation file.
//!
//! ```text
//! hl-latency <observations.jsonl>
//! ```
//!
//! Prints the [`hl_latency::Summary`] as JSON. Exit codes: 0 ok, 2 usage or
//! unreadable file.

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let (Some(path), None) = (args.next(), args.next()) else {
        eprintln!("usage: hl-latency <observations.jsonl>");
        return ExitCode::from(2);
    };
    let obs = match std::fs::File::open(&path)
        .map_err(hl_latency::ReadError::from)
        .and_then(|f| hl_latency::read_observations(std::io::BufReader::new(f)))
    {
        Ok(obs) => obs,
        Err(e) => {
            eprintln!("{path}: {e}");
            return ExitCode::from(2);
        }
    };
    let summary = hl_latency::summarize(&hl_latency::pair(obs));
    match serde_json::to_string_pretty(&summary) {
        Ok(s) => {
            println!("{s}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("serialise: {e}");
            ExitCode::from(2)
        }
    }
}
