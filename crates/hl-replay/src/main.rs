//! `hl-replay`: run the end-to-end replay harness and print its report.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use hl_replay::{
    generated_lines, load_lines, render_run, run_shuffled, write_artifacts, Arm, HostFacts,
    HostFactsNote, HostInfo, LoopbackAddr, ReplayError, RoundConfig, RunMeta, GIT_SHA,
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
  --out DIR          also write report.txt and raw histograms there; needs
                     --host-facts
  --host-facts FILE  host facts written by scripts/capture-host.sh on this
                     host; checked before anything runs, named in the report
                     and written to DIR/host-facts.txt
  --allow-missing-host-facts
                     local runs only: let --out go ahead without host facts;
                     the report says NOT CAPTURED
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
    host_facts: Option<PathBuf>,
    allow_missing_host_facts: bool,
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

/// Refuses option combinations that cannot give a meaningful run.
fn validate(a: &Args) -> Result<(), String> {
    if a.rounds == 0 {
        return Err("--rounds must be at least 1".to_owned());
    }
    if a.lines <= a.warmup {
        return Err(format!(
            "--lines ({}) must exceed --warmup ({}): nothing would be recorded",
            a.lines, a.warmup
        ));
    }
    if a.host_facts.is_some() && a.allow_missing_host_facts {
        return Err(
            "--host-facts and --allow-missing-host-facts contradict each other: give one"
                .to_owned(),
        );
    }
    if a.out.is_some() && a.host_facts.is_none() && !a.allow_missing_host_facts {
        return Err(
            "--out needs --host-facts FILE: run scripts/capture-host.sh FILE on this host first \
             (for a local run that is not a result, pass --allow-missing-host-facts)"
                .to_owned(),
        );
    }
    if a.distinct == 0 {
        return Err("--distinct must be at least 1".to_owned());
    }
    Ok(())
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
        host_facts: None,
        allow_missing_host_facts: false,
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
            "--allow-missing-host-facts" => {
                a.allow_missing_host_facts = true;
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
            "--host-facts",
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
            "--host-facts" => a.host_facts = Some(PathBuf::from(v)),
            "--sink-bind" => a.sink_bind = v.parse().map_err(|e| format!("--sink-bind: {e}"))?,
            _ => a.out = Some(PathBuf::from(v)),
        }
    }
    if no_pin {
        a.feeder_core = None;
        a.hot_core = None;
    }
    validate(&a)?;
    Ok(Some(a))
}

/// Checks the host-facts file, if one was given, before anything runs.
/// Returns the report's note and the checked facts, which are kept in memory
/// for the copy beside the report.
fn check_host_facts(
    args: &Args,
    host: &HostInfo,
) -> Result<(HostFactsNote, Option<HostFacts>), ReplayError> {
    match &args.host_facts {
        Some(path) => {
            let facts = HostFacts::load(path)?;
            facts.check_kernel(&host.kernel)?;
            let note = HostFactsNote::captured(path, &facts, args.feeder_core, args.hot_core);
            Ok((note, Some(facts)))
        }
        None if args.allow_missing_host_facts => Ok((HostFactsNote::Waived, None)),
        None => Ok((HostFactsNote::NotRequired, None)),
    }
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
    let host = HostInfo::detect();
    let (host_facts, checked_facts) = match check_host_facts(&args, &host) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("hl-replay: --host-facts: {e}");
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
        host,
        feeder_core: cfg.feeder_core,
        hot_core: cfg.hot_core,
        rounds: args.rounds,
        measured_per_round: cfg.measured,
        warmup_per_round: cfg.warmup,
        interval: cfg.interval,
        input,
        host_facts,
    };
    let report = render_run(&meta, &run);
    print!("{report}");
    if let Some(dir) = &args.out {
        if let Err(e) = write_artifacts(dir, &report, &run.arms) {
            eprintln!("hl-replay: --out: {e}");
            return ExitCode::FAILURE;
        }
        if let Some(facts) = &checked_facts {
            if let Err(e) = facts.write_to(dir) {
                eprintln!("hl-replay: --out: host-facts.txt: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    ExitCode::SUCCESS
}
