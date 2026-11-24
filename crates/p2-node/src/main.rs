//! `p2-node replay` and `p2-node gen`; see `--help` text in `USAGE`.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use hl_warm::{lock_memory, pin_current_thread, GateConfig, LockOutcome, WarmGate};
use p2_node::engines::{EchoEngine, NullEngine};
use p2_node::replay::{load_native_diffs, Replay, ReplayBlock, ReplayError};
use p2_node::timing::{run_passes, Timed, TimingConfig};

const USAGE: &str = "usage:
  p2-node replay --blocks <replica_cmds file> --diffs <node_raw_book_diffs batch file> --snapshot-height <H>
                 [--engine null|echo|hlexec --seed resting_<H>.jsonl --asset-meta asset_meta.json [--statuses F]]
                 [--passes N>=5] [--prewarm-passes N] [--window-blocks N]
                 [--core C] [--require-mlock] [--out DIR]
                 [--compare-only]  (stream decode, run, compare, drop per block: the k/N count
                 with memory flat in N; no warm gate, nothing timed)
  p2-node gen --blocks N --actions K --seed S --out-blocks F --out-diffs F
  p2-node warm-trial --config cold|warm|warm-no-pin|warm-no-arena|warm-no-prewarm
                 --blocks F --diffs F --prewarm-blocks F --prewarm-diffs F --n N [--prewarm R] [--core C]
                 (one trial per process; prints one CSV line: config,event,ns,...)
  p2-node native-times <node_fast_block_times file>
                 (native's own apply_duration per block: median/p90/p99/max/mean, ns)
engines: null = executes nothing (driver overhead, honest coverage); echo = returns native's
answers (harness oracle). Neither is an execution engine: no speed claim comes from them.
hlexec = lane 1's hl-exec engine, seeded from lane 1's resting-order seed (perp books only:
every perp asset in --asset-meta is seeded, empty books included; spot books are not held, so
actions on them are unsupported and their native diffs are not adopted). --statuses F (native's
order_statuses_by_block file) gives resync native's oid counter, rejected orders' oids included.
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
        Some("render-check") => render_check(&a),
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
    if !matches!(engine, "null" | "echo" | "hlexec") {
        return usage("--engine must be null, echo or hlexec");
    }
    let hl = if engine == "hlexec" {
        match load_hlexec(a, snapshot) {
            Ok(x) => Some(x),
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::from(1);
            }
        }
    } else {
        None
    };

    if a.0.iter().any(|x| x == "--compare-only") {
        return compare_only(a, bpath, dpath, snapshot, engine, hl.as_ref());
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
        let h = match p2_node::replay::height_at(snapshot, i) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::from(1);
            }
        };
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
    // Native's answers, copied only for the echo engine (they double the
    // decoded input's memory).
    let native: Vec<_> = if engine == "echo" {
        blocks
            .iter()
            .map(|b| (b.native_responses().to_vec(), b.native_diffs().to_vec()))
            .collect()
    } else {
        Vec::new()
    };
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
    let timed: Result<Timed, _> = match (engine, &hl) {
        ("hlexec", Some((table, seeded, st))) => run_passes(
            &mut || {
                p2_node::hlexec::HlExecEngine::new(seeded.clone(), table.clone())
                    .with_status_max_oids(st.clone())
            },
            &lines,
            &blocks,
            snapshot,
            &cfg,
        ),
        ("echo", _) => run_passes(
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
    if let Some((table, _, _)) = &hl {
        // Mismatched blocks by class; a block that touches a book the seed
        // does not hold is "unseeded-book" whatever else differs.
        let unseeded: std::collections::HashSet<u64> = blocks
            .iter()
            .filter(|b| p2_node::hlexec::touches_unseeded(table, b))
            .map(ReplayBlock::height)
            .collect();
        let mut by: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
        for (h, c) in &first_pass.mismatches {
            let k = if unseeded.contains(h) {
                "unseeded-book".to_owned()
            } else {
                format!("{c:?}")
            };
            *by.entry(k).or_default() += 1;
        }
        let _ = writeln!(
            report,
            "blocks touching an unseeded book: {} of {}; not-reproduced blocks by class:",
            unseeded.len(),
            blocks.len()
        );
        for (k, n) in &by {
            let _ = writeln!(report, "  {n:>6}  {k}");
        }
    }
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

/// Opens the blocks file as a stream of non-empty lines.
fn block_lines(bpath: &str) -> Result<impl Iterator<Item = Result<String, String>>, String> {
    let f = std::fs::File::open(bpath).map_err(|e| format!("{bpath}: {e}"))?;
    let bp = bpath.to_owned();
    Ok(BufReader::new(f)
        .lines()
        .map(move |l| l.map_err(|e| format!("{bp}: {e}")))
        .filter(|l| l.as_ref().map_or(true, |l| !l.trim().is_empty())))
}

/// `replay --compare-only`: the per-block k/N count, streamed. Each block is
/// decoded, run, compared and dropped, so memory stays flat in N (the timed
/// path keeps every decoded block for its passes). F2 is checked for the
/// whole input before the engine runs; F1's decoded-height check runs as
/// blocks arrive and refuses the run (no report) if it fails.
fn compare_only(
    args: &Args,
    bpath: &str,
    dpath: &str,
    snapshot: u64,
    engine: &str,
    hl: Option<&HlSetup>,
) -> ExitCode {
    let fail = |e: String| {
        eprintln!("{e}");
        ExitCode::from(1)
    };
    let mut diffs = match std::fs::File::open(dpath)
        .map_err(|e| e.to_string())
        .and_then(|f| load_native_diffs(BufReader::new(f)).map_err(|e| e.to_string()))
    {
        Ok(d) => d,
        Err(e) => return fail(format!("{dpath}: {e}")),
    };
    // Pre-pass: count blocks (F2 up front) and, for echo only, collect
    // native's answers (echo replays them; it is the harness oracle).
    let mut count = 0u64;
    let mut native = Vec::new();
    let lines = match block_lines(bpath) {
        Ok(l) => l,
        Err(e) => return fail(e),
    };
    for l in lines {
        let line = match l {
            Ok(line) => line,
            Err(e) => return fail(e),
        };
        count += 1;
        let height = match nth_height(snapshot, count) {
            Ok(h) => h,
            Err(e) => return fail(e),
        };
        if !diffs.contains_key(&height) {
            return fail(ReplayError::NoNativeDiffs { height }.to_string());
        }
        if engine == "echo" {
            match hl_block::decode_block(&line) {
                Ok(block) => native.push((
                    block
                        .actions()
                        .map(|x| x.outcome.to_json())
                        .collect::<Vec<_>>(),
                    diffs.get(&height).map(|d| d.1.clone()).unwrap_or_default(),
                )),
                Err(e) => return fail(format!("{bpath}:{count}: {e}")),
            }
        }
    }
    let lines = match block_lines(bpath) {
        Ok(l) => l,
        Err(e) => return fail(e),
    };
    let mut line_no = 0u64;
    let stream = lines.map(|l| {
        line_no += 1;
        let height = nth_height(snapshot, line_no)?;
        let line = l?;
        let block = hl_block::decode_block(&line).map_err(|e| format!("{bpath}:{line_no}: {e}"))?;
        ReplayBlock::new(height, block, diffs.remove(&height)).map_err(|e| e.to_string())
    });
    let mut unseeded = std::collections::HashSet::new();
    let each = |blk: &ReplayBlock, _: bool| {
        if let Some((table, _, _)) = hl {
            if p2_node::hlexec::touches_unseeded(table, blk) {
                unseeded.insert(blk.height());
            }
        }
    };
    let run = match (engine, hl) {
        ("hlexec", Some((table, seeded, st))) => Replay::run_stream(
            &mut p2_node::hlexec::HlExecEngine::new(seeded.clone(), table.clone())
                .with_status_max_oids(st.clone()),
            stream,
            snapshot,
            each,
        ),
        ("echo", _) => Replay::run_stream(&mut EchoEngine::new(native), stream, snapshot, each),
        _ => Replay::run_stream(&mut NullEngine, stream, snapshot, each),
    };
    let run = match run {
        Ok(run) => run,
        Err(e) => return fail(e.to_string()),
    };
    let mut report = String::new();
    let _ = writeln!(
        report,
        "engine {engine}  blocks {}  compare-only: nothing timed",
        run.blocks
    );
    report.push_str(&run.to_text());
    if hl.is_some() {
        report.push_str(&class_report(&run, &unseeded));
    }
    let _ = writeln!(
        report,
        "{}",
        p2_node::replay::verdict_label(run.reproduced, run.blocks, None)
    );
    print!("{report}");
    if let Err(e) = write_out(args, &report, &heights_tsv(&run, &unseeded)) {
        return fail(e);
    }
    ExitCode::SUCCESS
}

/// `snapshot + n` for the n-th (1-based) block line, checked.
fn nth_height(snapshot: u64, n: u64) -> Result<u64, String> {
    snapshot.checked_add(n).ok_or_else(|| {
        ReplayError::HeightOverflow {
            snapshot,
            index: usize::try_from(n.saturating_sub(1)).unwrap_or(usize::MAX),
        }
        .to_string()
    })
}

/// With `--out DIR`: the report and the per-height verdicts.
fn write_out(args: &Args, report: &str, heights: &str) -> Result<(), String> {
    let Some(out) = args.get("--out").map(PathBuf::from) else {
        return Ok(());
    };
    std::fs::create_dir_all(&out)
        .and_then(|()| std::fs::write(out.join("report.txt"), report))
        .and_then(|()| std::fs::write(out.join("heights.tsv"), heights))
        .map_err(|e| format!("{}: {e}", out.display()))
}

/// One line per block: height, verdict (reproduced / mismatch class), and
/// whether native's diff list was empty, for reconciling runs by height.
fn heights_tsv(run: &Replay, unseeded: &std::collections::HashSet<u64>) -> String {
    let mut rows: Vec<(u64, String)> = run
        .reproduced_heights
        .iter()
        .map(|h| {
            let zero = run.reproduced_zero_native_diffs.contains(h);
            (
                *h,
                format!(
                    "reproduced\t{}",
                    if zero { "zero-native-diffs" } else { "-" }
                ),
            )
        })
        .collect();
    for (h, cause) in &run.mismatches {
        let class = if unseeded.contains(h) {
            "unseeded-book".to_owned()
        } else {
            format!("{cause:?}")
        };
        rows.push((*h, format!("{class}\t-")));
    }
    rows.sort_by_key(|r| r.0);
    let mut s = String::from("height\tverdict\tnative_diffs\n");
    for (h, v) in rows {
        let _ = writeln!(s, "{h}\t{v}");
    }
    s
}

/// Not-reproduced blocks by class; a block touching a book the seed does not
/// hold is "unseeded-book" whatever else differs.
fn class_report(run: &Replay, unseeded: &std::collections::HashSet<u64>) -> String {
    let mut report = String::new();
    let mut by: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    for (height, cause) in &run.mismatches {
        let key = if unseeded.contains(height) {
            "unseeded-book".to_owned()
        } else {
            format!("{cause:?}")
        };
        *by.entry(key).or_default() += 1;
    }
    let _ = writeln!(
        report,
        "blocks touching an unseeded book: {} of {}; not-reproduced blocks by class:",
        unseeded.len(),
        run.blocks
    );
    for (key, count) in &by {
        let _ = writeln!(report, "  {count:>6}  {key}");
    }
    report
}

/// Asset table, seeded engine and native's highest oid per height (from
/// `--statuses`, empty without it).
type HlSetup = (
    p2_node::hlexec::AssetTable,
    hl_exec::Engine,
    std::collections::HashMap<u64, u64>,
);

/// The asset table (perp assets only: the seed holds perp books) and lane
/// 1's resting-order seed, checked against `--snapshot-height`; with
/// `--statuses`, native's order-status file for resync's oid counter.
fn load_hlexec(a: &Args, snapshot: u64) -> Result<HlSetup, String> {
    let (Some(sp), Some(mp)) = (a.get("--seed"), a.get("--asset-meta")) else {
        return Err("--engine hlexec needs --seed and --asset-meta".into());
    };
    let all = std::fs::read_to_string(mp)
        .map_err(|e| e.to_string())
        .and_then(|s| p2_node::hlexec::AssetTable::from_json(&s))
        .map_err(|e| format!("{mp}: {e}"))?;
    // Spot is 10000..100000; perps are below it, builder perps above it.
    let table = all.filtered(|m| !(10_000..100_000).contains(&m.asset));
    let f = std::fs::File::open(sp).map_err(|e| format!("{sp}: {e}"))?;
    let seed =
        p2_node::hlexec::RestingSeed::read(BufReader::new(f)).map_err(|e| format!("{sp}: {e}"))?;
    if seed.height != snapshot {
        return Err(format!(
            "{sp}: seed height {} but --snapshot-height {snapshot}",
            seed.height
        ));
    }
    if let Some(o) = seed.orders.iter().find(|o| table.get(o.asset).is_none()) {
        return Err(format!(
            "{sp}: oid {} on asset {} not in the perp asset table",
            o.oid.0, o.asset
        ));
    }
    eprintln!(
        "hlexec seed height {}: {} resting orders, {} perp books seeded, next_oid {:?}",
        seed.height,
        seed.orders.len(),
        table.ids().len(),
        seed.next_oid.map(|o| o.0)
    );
    let max = seed.orders.iter().map(|o| o.oid.0).max().unwrap_or(0);
    let e = hl_exec::Engine::from_l4_snapshot(
        &table.ids(),
        seed.orders.iter().copied(),
        Some(seed.next_oid.unwrap_or(hl_exec::Oid(max + 1))),
        hl_exec::Capacity::default(),
        hl_exec::EngineConfig::default(),
    )
    .map_err(|e| format!("{sp}: seed does not load into hl-exec: {e:?}"))?;
    let st = match a.get("--statuses") {
        Some(path) => {
            let file = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
            let max = p2_node::hlexec::load_status_max_oids(BufReader::new(file))
                .map_err(|e| format!("{path}: {e}"))?;
            eprintln!("hlexec statuses: highest oid for {} heights", max.len());
            max
        }
        None => std::collections::HashMap::new(),
    };
    Ok((table, e, st))
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
    let contiguous = v
        .windows(2)
        .all(|w| w[0].height.checked_add(1) == Some(w[1].height));
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

/// Renderer check: for every order-book action in a `replica_cmds` file
/// (plain or gzip-free JSON lines), parse native's response with the action
/// and asset metadata, render it with the hl-exec adapter's renderer and
/// count value-equal / different / not rendered (a response class the
/// engine never produces, e.g. a margin error).
fn render_check(a: &Args) -> ExitCode {
    use std::collections::BTreeMap;
    use std::io::BufRead as _;
    let (Some(bp), Some(mp)) = (a.get("--blocks"), a.get("--asset-meta")) else {
        return usage("render-check --blocks FILE --asset-meta asset_meta.json");
    };
    let meta = match std::fs::read_to_string(mp)
        .map_err(|e| e.to_string())
        .and_then(|s| p2_node::hlexec::AssetTable::from_json(&s))
    {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{mp}: {e}");
            return ExitCode::from(1);
        }
    };
    let f = match std::fs::File::open(bp) {
        Ok(f) => BufReader::new(f),
        Err(e) => {
            eprintln!("{bp}: {e}");
            return ExitCode::from(1);
        }
    };
    let (mut same, mut diff, mut skipped) = (0u64, 0u64, 0u64);
    let mut diff_by: BTreeMap<String, (u64, String)> = BTreeMap::new();
    for (i, line) in f.lines().enumerate() {
        let Ok(line) = line else { break };
        let b = match hl_block::decode_block(&line) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("{bp}:{}: {e}", i + 1);
                return ExitCode::from(1);
            }
        };
        for act in b.actions() {
            if !matches!(
                act.action_type.as_str(),
                "order" | "cancel" | "cancelByCloid" | "modify" | "batchModify" | "noop"
            ) {
                continue;
            }
            let native = act.outcome.to_json();
            let ua = p2_node::engine::UserAction::from_signed(act);
            match p2_node::hlexec::roundtrip(&meta, &ua, &native) {
                Some(true) => same += 1,
                Some(false) => {
                    diff += 1;
                    let e = diff_by
                        .entry(format!(
                            "{} {}",
                            act.action_type,
                            hl_block::census::response_classes(&act.outcome)
                                .first()
                                .cloned()
                                .unwrap_or_default()
                        ))
                        .or_insert((0, native.to_string()));
                    e.0 += 1;
                }
                None => skipped += 1,
            }
        }
    }
    println!("rendered value-equal {same}, different {diff}, not rendered (engine never answers this class) {skipped}");
    for (k, (n, ex)) in &diff_by {
        println!(
            "{n:>10}  {k}\n            e.g. {}",
            ex.chars().take(300).collect::<String>()
        );
    }
    ExitCode::SUCCESS
}
