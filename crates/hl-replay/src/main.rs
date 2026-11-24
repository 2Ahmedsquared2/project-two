//! `hl-replay`: run the end-to-end replay harness and print its report.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use hl_replay::{
    generated_lines, load_lines, render_run, run_shuffled, write_artifacts, Arm, HostInfo,
    LoopbackAddr, RoundConfig, RunMeta, GIT_SHA,
};

const USAGE: &str = "\
usage: hl-replay [options]

Replays node lines through reader -> parser -> book -> decision -> signer ->
send, timing each line from seen (t0) to parsed (t1) to bytes handed to a
loopback socket (t2), per arm, rounds interleaved.

  --arms LIST        comma-separated arms, or: runtime (default), detect, all
                     runtime: busy-poll tokio-current-thread tokio-multi-thread
                              compio compio-sqpoll
                     detect:  file-busy-poll file-poll-10us file-poll-100us
                              file-inotify
  --rounds N         rounds per arm, interleaved, each round in its own
                     random arm order (default 5)
  --seed N           seed for the arm orders (default: from the clock)
  --lines N          lines per round, warmup included (default 11000)
  --warmup N         lines per round not recorded (default 1000)
  --interval-us N    microseconds between lines, open loop (default 1000)
  --input FILE       replay recorded node lines (cycled) instead of generated
  --events N         events per generated line (default 20)
  --distinct N       distinct generated lines (default 64)
  --feeder-core N    core for the feeder and sink (default 0)
  --hot-core N       core for the hot thread (default 1)
  --no-pin           pin nothing
  --tail-dir DIR     where detect arms tail their file (default /dev/shm)
  --sink-bind ADDR   sink listen address; loopback only (default 127.0.0.1:0)
  --out DIR          also write report.txt and raw histograms there
  --help             this text
";

struct Args {
    arms: Vec<Arm>,
    rounds: u32,
    lines: u64,
    warmup: u64,
    interval_us: u64,
    input: Option<PathBuf>,
    events: usize,
    distinct: usize,
    feeder_core: Option<usize>,
    hot_core: Option<usize>,
    tail_dir: PathBuf,
    sink_bind: LoopbackAddr,
    out: Option<PathBuf>,
    seed: u64,
}

fn parse_arms(list: &str) -> Result<Vec<Arm>, String> {
    let mut arms = Vec::new();
    for name in list.split(',').filter(|s| !s.is_empty()) {
        match name {
            "runtime" => arms.extend(Arm::RUNTIME),
            "detect" => arms.extend(Arm::DETECT),
            "all" => {
                arms.extend(Arm::RUNTIME);
                arms.extend(Arm::DETECT);
            }
            one => arms.push(one.parse::<Arm>().map_err(|e| e.to_string())?),
        }
    }
    if arms.is_empty() {
        return Err("--arms: no arms named".to_owned());
    }
    Ok(arms)
}

fn num<T: std::str::FromStr>(flag: &str, value: &str) -> Result<T, String> {
    value
        .parse()
        .map_err(|_| format!("{flag}: {value:?} is not a valid number"))
}

/// `Ok(None)` means `--help`.
fn parse_args(argv: &[String]) -> Result<Option<Args>, String> {
    let mut a = Args {
        arms: Arm::RUNTIME.to_vec(),
        rounds: 5,
        lines: 11_000,
        warmup: 1_000,
        interval_us: 1_000,
        input: None,
        events: 20,
        distinct: 64,
        feeder_core: Some(0),
        hot_core: Some(1),
        tail_dir: PathBuf::from("/dev/shm"),
        sink_bind: LoopbackAddr::ephemeral(),
        out: None,
        seed: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::from(d.subsec_nanos())),
    };
    let mut no_pin = false;
    let mut it = argv.iter();
    while let Some(flag) = it.next() {
        let flag = flag.as_str();
        match flag {
            "--help" | "-h" => return Ok(None),
            "--no-pin" => {
                no_pin = true;
                continue;
            }
            _ => {}
        }
        let known = [
            "--arms",
            "--rounds",
            "--lines",
            "--warmup",
            "--interval-us",
            "--input",
            "--events",
            "--distinct",
            "--feeder-core",
            "--hot-core",
            "--tail-dir",
            "--sink-bind",
            "--out",
            "--seed",
        ];
        if !known.contains(&flag) {
            return Err(format!("unknown option {flag:?}"));
        }
        let v = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag {
            "--arms" => a.arms = parse_arms(v)?,
            "--rounds" => a.rounds = num(flag, v)?,
            "--seed" => a.seed = num(flag, v)?,
            "--lines" => a.lines = num(flag, v)?,
            "--warmup" => a.warmup = num(flag, v)?,
            "--interval-us" => a.interval_us = num(flag, v)?,
            "--input" => a.input = Some(PathBuf::from(v)),
            "--events" => a.events = num(flag, v)?,
            "--distinct" => a.distinct = num(flag, v)?,
            "--feeder-core" => a.feeder_core = Some(num(flag, v)?),
            "--hot-core" => a.hot_core = Some(num(flag, v)?),
            "--tail-dir" => a.tail_dir = PathBuf::from(v),
            "--sink-bind" => a.sink_bind = v.parse().map_err(|e| format!("--sink-bind: {e}"))?,
            _ => a.out = Some(PathBuf::from(v)),
        }
    }
    if no_pin {
        a.feeder_core = None;
        a.hot_core = None;
    }
    if a.rounds == 0 {
        return Err("--rounds must be at least 1".to_owned());
    }
    if a.lines <= a.warmup {
        return Err(format!(
            "--lines ({}) must exceed --warmup ({}): nothing would be recorded",
            a.lines, a.warmup
        ));
    }
    if a.distinct == 0 {
        return Err("--distinct must be at least 1".to_owned());
    }
    Ok(Some(a))
}

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse_args(&raw) {
        Ok(Some(a)) => a,
        Ok(None) => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("hl-replay: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let (lines, input) = if let Some(path) = &args.input {
        match load_lines(path) {
            Ok(l) => {
                let what = format!("recorded, {} ({} lines, cycled)", path.display(), l.len());
                (l, what)
            }
            Err(e) => {
                eprintln!("hl-replay: --input: {e}");
                return ExitCode::from(2);
            }
        }
    } else {
        let l = generated_lines(args.distinct, args.events);
        let what = format!(
            "generated, {} distinct lines of {} events ({} bytes each), cycled",
            l.len(),
            args.events,
            l.first().map_or(0, String::len)
        );
        (l, what)
    };
    let cfg = RoundConfig {
        lines: Arc::new(lines),
        measured: args.lines - args.warmup,
        warmup: args.warmup,
        interval: Duration::from_micros(args.interval_us),
        feeder_core: args.feeder_core,
        hot_core: args.hot_core,
        tail_dir: args.tail_dir,
        sink_bind: args.sink_bind,
    };
    let run = match run_shuffled(&args.arms, args.rounds, &cfg, args.seed) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("hl-replay: {e}");
            return ExitCode::FAILURE;
        }
    };
    let meta = RunMeta {
        commit: GIT_SHA.to_owned(),
        host: HostInfo::detect(),
        feeder_core: cfg.feeder_core,
        hot_core: cfg.hot_core,
        rounds: args.rounds,
        measured_per_round: cfg.measured,
        warmup_per_round: cfg.warmup,
        interval: cfg.interval,
        input,
        host_facts: hl_replay::HostFactsNote::NotRequired,
    };
    let report = render_run(&meta, &run);
    print!("{report}");
    if let Some(dir) = &args.out {
        if let Err(e) = write_artifacts(dir, &report, &run.arms) {
            eprintln!("hl-replay: --out: {e}");
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}
