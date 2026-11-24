//! `p2-node replay` and `p2-node gen`; see `--help` text in `USAGE`.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::BufReader;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use hl_warm::{lock_memory, pin_current_thread, GateConfig, LockOutcome, WarmGate};
use p2_node::engines::{EchoEngine, NullEngine};
use p2_node::replay::{load_native_diffs, ReplayBlock};
use p2_node::timing::{run_passes, Timed, TimingConfig};

const USAGE: &str = "usage:
  p2-node replay --blocks <replica_cmds file> --diffs <node_raw_book_diffs batch file> --snapshot-height <H>
                 [--engine null|echo] [--passes N>=5] [--prewarm-passes N] [--window-blocks N]
                 [--core C] [--require-mlock] [--out DIR]
  p2-node gen --blocks N --actions K --seed S --out-blocks F --out-diffs F
  p2-node warm-trial --config cold|warm|warm-no-pin|warm-no-arena|warm-no-prewarm
                 --blocks F --diffs F --prewarm-blocks F --prewarm-diffs F --n N [--prewarm R] [--core C]
                 (one trial per process; prints one CSV line: config,event,ns,...)
  p2-node native-times <node_fast_block_times file>
                 (native's own apply_duration per block: median/p90/p99/max/mean, ns)
engines: null = executes nothing (driver overhead, honest coverage); echo = returns native's
answers (harness oracle). Neither is an execution engine: no speed claim comes from them.
exit: 0 ok, 1 run error, 2 usage";

struct Args(Vec<String>);

impl Args {
    fn get(&self, k: &str) -> Option<&str> {
        self.0
            .iter()
            .position(|a| a == k)
            .and_then(|i| self.0.get(i + 1))
            .map(String::as_str)
    }
    fn num(&self, k: &str, default: Option<u64>) -> Result<u64, String> {
        match self.get(k) {
            Some(v) => v.parse().map_err(|_| format!("{k}: not a number: {v}")),
            None => default.ok_or_else(|| format!("missing {k}")),
        }
    }
}

fn usage(e: &str) -> ExitCode {
    eprintln!("{e}\n{USAGE}");
    ExitCode::from(2)
}

fn now_ns(t0: Instant) -> u64 {
    u64::try_from(t0.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn main() -> ExitCode {
    let a = Args(std::env::args().skip(1).collect());
    match a.0.first().map(String::as_str) {
        Some("replay") => replay(&a),
        Some("gen") => gen(&a),
        Some("warm-trial") => warm_trial(&a),
        Some("native-times") => native_times(&a),
        _ => usage("no command"),
    }
}

#[allow(clippy::many_single_char_names)]
fn gen(a: &Args) -> ExitCode {
    let (Ok(n), Ok(k), Ok(seed)) = (
        a.num("--blocks", None),
        a.num("--actions", None),
        a.num("--seed", Some(1)),
    ) else {
        return usage("gen needs --blocks and --actions");
    };
    let (Some(ob), Some(od)) = (a.get("--out-blocks"), a.get("--out-diffs")) else {
        return usage("gen needs --out-blocks and --out-diffs");
    };
    let (b, d) = p2_node::gen::generate(n, k, seed);
    if std::fs::write(ob, b)
        .and_then(|()| std::fs::write(od, d))
        .is_err()
    {
        eprintln!("write failed");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

#[allow(clippy::too_many_lines)]
fn replay(a: &Args) -> ExitCode {
    let (Some(bpath), Some(dpath)) = (a.get("--blocks"), a.get("--diffs")) else {
        return usage("replay needs --blocks and --diffs");
    };
    let parsed = (|| -> Result<_, String> {
        Ok((
            a.num("--snapshot-height", None)?,
            a.num("--passes", Some(10))?,
            a.num("--prewarm-passes", Some(2))?,
            a.num("--window-blocks", Some(0))?,
            a.get("--core")
                .map(str::parse::<usize>)
                .transpose()
                .map_err(|e| format!("--core: {e}"))?,
        ))
    })();
    let (snapshot, passes, prewarm, window, core) = match parsed {
        Ok(v) => v,
        Err(e) => return usage(&e),
    };
    let Ok(passes) = u32::try_from(passes) else {
        return usage("--passes too large");
    };
    if passes < 5 {
        return usage("--passes must be >= 5");
    }
    let Ok(prewarm) = u32::try_from(prewarm) else {
        return usage("--prewarm-passes too large");
    };
    let engine = a.get("--engine").unwrap_or("null");
    if !matches!(engine, "null" | "echo") {
        return usage("--engine must be null or echo (hl-exec adapter lands with lane 1)");
    }

    // Warm-up steps; the gate stays closed (and nothing is timed) until all pass.
    let t0 = Instant::now();
    let mut gate = WarmGate::new(GateConfig {
        steps: vec!["pin", "mlock", "load", "prewarm"],
        min_window_ns: 0,
        min_window_events: 0,
    });
    match core {
        Some(c) => match pin_current_thread(c) {
            Ok(()) => drop(gate.pass("pin", now_ns(t0))),
            Err(e) => gate.fail("pin", &e.to_string(), now_ns(t0)),
        },
        None => drop(gate.pass("pin", now_ns(t0))),
    }
    let mut warm = String::new();
    let _ = writeln!(warm, "pin core: {core:?}");

    let lines: Vec<String> = match std::fs::read_to_string(bpath) {
        Ok(s) => s
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(str::to_owned)
            .collect(),
        Err(e) => {
            eprintln!("{bpath}: {e}");
            return ExitCode::from(1);
        }
    };
    let diffs: HashMap<u64, (u64, Vec<_>)> = match std::fs::File::open(dpath).map(BufReader::new) {
        Ok(r) => match load_native_diffs(r) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("{dpath}: {e}");
                return ExitCode::from(1);
            }
        },
        Err(e) => {
            eprintln!("{dpath}: {e}");
            return ExitCode::from(1);
        }
    };
    let mut blocks = Vec::with_capacity(lines.len());
    for (i, l) in lines.iter().enumerate() {
        let h = snapshot + 1 + u64::try_from(i).unwrap_or(u64::MAX);
        let b = match hl_block::decode_block(l) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("{bpath}:{}: {e}", i + 1);
                return ExitCode::from(1);
            }
        };
        match ReplayBlock::new(h, b, diffs.get(&h).cloned()) {
            Ok(rb) => blocks.push(rb),
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::from(1);
            }
        }
    }
    let _ = gate.pass("load", now_ns(t0));
    // Locked after loading. Refused on a host without unlimited RLIMIT_MEMLOCK;
    // recorded here, REQUIRED on the bench host (docs/bench/METHOD.md: the run
    // passes --require-mlock there).
    let lock = lock_memory();
    let _ = writeln!(warm, "{lock}");
    if a.0.iter().any(|x| x == "--require-mlock") && matches!(lock, LockOutcome::Refused(_)) {
        gate.fail("mlock", &lock.to_string(), now_ns(t0));
    } else {
        let _ = gate.pass("mlock", now_ns(t0));
    }
    let native: Vec<_> = blocks
        .iter()
        .map(|b| (b.native_responses().to_vec(), b.native_diffs().to_vec()))
        .collect();
    let cfg = TimingConfig {
        passes,
        prewarm_passes: prewarm,
        window_blocks: window,
    };
    let _ = gate.pass("prewarm", now_ns(t0));
    if !gate.on_event(now_ns(t0)).eq(&hl_warm::Phase::Measured) {
        eprintln!("warm gate closed:\n{}", gate.report());
        return ExitCode::from(1);
    }
    let timed: Result<Timed, _> = match engine {
        "echo" => run_passes(
            &mut || EchoEngine::new(native.clone()),
            &lines,
            &blocks,
            snapshot,
            &cfg,
        ),
        _ => run_passes(&mut || NullEngine, &lines, &blocks, snapshot, &cfg),
    };
    let timed = match timed {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    let _ = write!(warm, "{}", gate.report());
    let first_pass = &timed.passes[0].replay;
    let mut report = String::new();
    let _ = writeln!(report, "engine {engine}  blocks {}  passes {passes}  prewarm {prewarm}  window {window}  core {core:?}", blocks.len());
    report.push_str(&first_pass.to_text());
    report.push_str(&timed.summary().to_text());
    // Like-for-like also needs the end-state check, which this CLI does not
    // run yet: until it does, every speed row stays "not like-for-like".
    let end_state_checked = first_pass.end_state == Some(true);
    let like_for_like = end_state_checked
        && timed.outputs_matched.0 == timed.outputs_matched.1
        && first_pass.actions_executed == first_pass.actions
        && engine != "echo"
        && engine != "null";
    if !like_for_like {
        let _ = writeln!(
            report,
            "NOT a like-for-like speed claim: engine {engine}, {}, actions executed {}/{}",
            p2_node::replay::verdict_label(timed.outputs_matched.0, timed.outputs_matched.1, None),
            first_pass.actions_executed,
            first_pass.actions
        );
    }
    print!("{report}");
    if let Some(out) = a.get("--out").map(PathBuf::from) {
        let w = std::fs::create_dir_all(&out)
            .and_then(|()| std::fs::write(out.join("report.txt"), &report))
            .and_then(|()| std::fs::write(out.join("blocks.csv"), timed.to_csv()))
            .and_then(|()| std::fs::write(out.join("warm.txt"), &warm));
        if let Err(e) = w {
            eprintln!("{}: {e}", out.display());
            return ExitCode::from(1);
        }
    }
    if matches!(lock, LockOutcome::Refused(_)) {
        eprintln!("note: {lock}");
    }
    ExitCode::SUCCESS
}

fn load_pair(
    bp: &str,
    dp: &str,
) -> Result<(Vec<String>, Vec<Vec<hl_node_convert::node::BookDiff>>), String> {
    let lines: Vec<String> = std::fs::read_to_string(bp)
        .map_err(|e| format!("{bp}: {e}"))?
        .lines()
        .map(str::to_owned)
        .collect();
    let f = std::fs::File::open(dp).map_err(|e| format!("{dp}: {e}"))?;
    let mut d = load_native_diffs(BufReader::new(f)).map_err(|e| format!("{dp}: {e}"))?;
    let diffs = (1..=u64::try_from(lines.len()).unwrap_or(0))
        .map(|h| d.remove(&h).map(|x| x.1).unwrap_or_default())
        .collect();
    Ok((lines, diffs))
}

/// One cold-or-warm trial in this (fresh) process.
fn warm_trial(a: &Args) -> ExitCode {
    use p2_node::warmbench::{hot, steps};
    let Some(config) = a.get("--config") else {
        return usage("--config");
    };
    let Some((pin, arena_on, prewarm_on)) = steps(config) else {
        return usage("unknown --config");
    };
    let (Some(bp), Some(dp), Some(pbp), Some(pdp)) = (
        a.get("--blocks"),
        a.get("--diffs"),
        a.get("--prewarm-blocks"),
        a.get("--prewarm-diffs"),
    ) else {
        return usage("warm-trial needs --blocks --diffs --prewarm-blocks --prewarm-diffs");
    };
    let (Ok(n), Ok(rounds), Ok(core)) = (
        a.num("--n", None),
        a.num("--prewarm", Some(20)),
        a.num("--core", Some(0)),
    ) else {
        return usage("bad --n/--prewarm/--core");
    };
    let Ok(core) = usize::try_from(core) else {
        return usage("--core");
    };
    // Loading input is not part of any configuration: every trial reads its
    // files before the first measured block.
    let (lines, diffs) = match load_pair(bp, dp) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    let n = usize::try_from(n).unwrap_or(usize::MAX).min(lines.len());
    let mut times = Vec::with_capacity(n);
    let mut pin_state = "off".to_owned();
    if pin {
        pin_state = match pin_current_thread(core) {
            Ok(()) => format!("core{core}"),
            Err(e) => format!("failed:{e}"),
        };
    }
    let mut arena = if arena_on {
        hl_warm::Arena::new(8 << 20).ok()
    } else {
        None
    };
    let backing = arena
        .as_ref()
        .map_or("none".to_owned(), |x| format!("{:?}", x.backing()));
    let mut sink = 0u64;
    if prewarm_on {
        let (pl, pd) = match load_pair(pbp, pdp) {
            Ok(x) => x,
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::from(1);
            }
        };
        for _ in 0..rounds {
            let mut book = hl_book_ref::Book::new();
            for (l, d) in pl.iter().zip(&pd) {
                sink ^= hot(
                    l,
                    d,
                    &mut book,
                    arena.as_mut().map(hl_warm::Arena::as_mut_slice),
                )
                .1;
            }
        }
    }
    let mut book = hl_book_ref::Book::new();
    let before = hl_warm::ThreadUsage::now().ok();
    for (l, d) in lines.iter().zip(&diffs).take(n) {
        let (ns, c) = hot(
            l,
            d,
            &mut book,
            arena.as_mut().map(hl_warm::Arena::as_mut_slice),
        );
        sink ^= c;
        times.push(ns);
    }
    // Off Linux there is no per-thread getrusage: print "na", never zeros.
    let usage = match (before, hl_warm::ThreadUsage::now().ok()) {
        (Some(b), Some(n)) => {
            let d = n.since(&b);
            format!(
                "{},{},{}",
                d.minor_faults, d.major_faults, d.involuntary_switches
            )
        }
        _ => "na,na,na".to_owned(),
    };
    let mut out = String::new();
    for (i, t) in times.iter().enumerate() {
        let _ = writeln!(out, "{config},{i},{t},{pin_state},{backing},{usage},{sink}");
    }
    print!("{out}");
    ExitCode::SUCCESS
}

/// Native's own per-block `apply_duration` (from `node_fast_block_times`).
fn native_times(a: &Args) -> ExitCode {
    let Some(path) = a.0.get(1) else {
        return usage("native-times needs a file");
    };
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{path}: {e}");
            return ExitCode::from(1);
        }
    };
    let mut v = Vec::new();
    for (i, l) in text
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
    {
        match p2_node::native_times::parse_line(l) {
            Ok(t) => v.push(t),
            Err(e) => {
                eprintln!("{path}:{}: {e}", i + 1);
                return ExitCode::from(1);
            }
        }
    }
    let contiguous = v.windows(2).all(|w| w[1].height == w[0].height + 1);
    let mut ns: Vec<u64> = v.iter().map(|t| t.apply_ns).collect();
    ns.sort_unstable();
    let p = |m| p2_node::timing::percentile(&ns, m).unwrap_or(0);
    let n = u128::try_from(ns.len()).unwrap_or(1).max(1);
    let mean = ns.iter().map(|&x| u128::from(x)).sum::<u128>() / n;
    println!(
        "blocks {}, heights {}..={}, contiguous {}",
        v.len(),
        v.first().map_or(0, |t| t.height),
        v.last().map_or(0, |t| t.height),
        if contiguous { "yes" } else { "NO" }
    );
    println!(
        "apply_duration ns: median {} p90 {} p99 {} max {} mean {}",
        p(500),
        p(900),
        p(990),
        ns.last().copied().unwrap_or(0),
        mean
    );
    ExitCode::SUCCESS
}
