//! Timed replay passes.
//!
//! Each counted pass starts from a fresh engine and replays every block. Per
//! block, two monotonic (`Instant`) intervals:
//! - `exec`: around `execute_block` only;
//! - `parse_exec`: from the raw `replica_cmds` line through decode, building
//!   the engine input, and `execute_block`.
//!
//! The comparison with native runs after both stamps and is not timed. A
//! block's samples count only when the gate is open AND the block was
//! reproduced in that pass. Before the counted passes, `prewarm_passes` full
//! passes run untimed (code pages, caches, branch predictors, lazy statics);
//! inside each counted pass a [`WarmGate`] keeps the first `window_blocks`
//! blocks out. Pinning and memory locking are the caller's (binary's) steps.

use std::fmt::Write as _;
use std::time::Instant;

use hl_warm::{GateConfig, Phase, WarmGate};

use crate::engine::{Engine, UserAction};
use crate::replay::{validate, Replay, ReplayBlock, ReplayError};

/// Pass settings.
#[derive(Clone, Copy, Debug)]
pub struct TimingConfig {
    /// Counted passes (at least 5, so the worst case is a range).
    pub passes: u32,
    /// Untimed full passes before the first counted one.
    pub prewarm_passes: u32,
    /// Blocks at the start of each counted pass that are warm-up.
    pub window_blocks: u64,
}

/// Why passes could not run.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TimingError {
    /// Fewer than 5 passes.
    #[error("need at least 5 passes, got {0}")]
    TooFewPasses(u32),
    /// Raw lines and replay blocks differ in number.
    #[error("{lines} lines but {blocks} blocks")]
    Length {
        /// Lines.
        lines: usize,
        /// Blocks.
        blocks: usize,
    },
    /// F1/F2 refusal of the input.
    #[error(transparent)]
    Input(#[from] ReplayError),
    /// F1: the raw line executed is not the block compared against.
    #[error("line {index}: block time {line_ns} ns is not the block compared (height {height}, block time {block_ns} ns)")]
    Mismatch {
        /// Index.
        index: usize,
        /// Height compared.
        height: u64,
        /// Block time of the executed line.
        line_ns: u64,
        /// Block time of the compared block.
        block_ns: u64,
    },
    /// A line failed to decode during a timed pass.
    #[error("line {0} failed to decode in a timed pass: {1}")]
    Decode(usize, String),
}

/// One timed block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Row {
    /// Height.
    pub height: u64,
    /// Gate phase.
    pub phase: Phase,
    /// Reproduced in this pass.
    pub reproduced: bool,
    /// Actions in the block.
    pub actions: u32,
    /// `execute_block` ns.
    pub exec_ns: u64,
    /// Decode + input + `execute_block` ns.
    pub parse_exec_ns: u64,
}

/// One counted pass.
#[derive(Clone, Debug, Default)]
pub struct Pass {
    /// Blocks reproduced.
    pub reproduced: u64,
    /// Blocks dropped as warm-up.
    pub excluded_warmup: u64,
    /// `exec` samples of measured, reproduced blocks.
    pub exec_ns: Vec<u64>,
    /// `parse_exec` samples of the same blocks.
    pub parse_exec_ns: Vec<u64>,
    /// Every block, for the per-block CSV.
    pub rows: Vec<Row>,
    /// The pass's replay report.
    pub replay: Replay,
}

/// All counted passes.
#[derive(Clone, Debug)]
pub struct Timed {
    /// Passes in run order.
    pub passes: Vec<Pass>,
    /// (min over passes of blocks reproduced, blocks).
    pub state_identical: (u64, u64),
}

fn ns(t: Instant) -> u64 {
    u64::try_from(t.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// Runs `prewarm_passes` untimed and then `passes` counted passes, each with a
/// fresh engine from `new_engine`.
pub fn run_passes<E: Engine>(
    new_engine: &mut impl FnMut() -> E,
    lines: &[String],
    blocks: &[ReplayBlock],
    snapshot: u64,
    cfg: &TimingConfig,
) -> Result<Timed, TimingError> {
    if cfg.passes < 5 {
        return Err(TimingError::TooFewPasses(cfg.passes));
    }
    if lines.len() != blocks.len() {
        return Err(TimingError::Length {
            lines: lines.len(),
            blocks: blocks.len(),
        });
    }
    validate(blocks, snapshot)?;
    for _ in 0..cfg.prewarm_passes {
        one_pass(new_engine(), lines, blocks, snapshot, cfg.window_blocks)?;
    }
    let mut passes = Vec::new();
    for _ in 0..cfg.passes {
        passes.push(one_pass(
            new_engine(),
            lines,
            blocks,
            snapshot,
            cfg.window_blocks,
        )?);
    }
    let min = passes.iter().map(|p| p.reproduced).min().unwrap_or(0);
    Ok(Timed {
        passes,
        state_identical: (min, u64::try_from(blocks.len()).unwrap_or(u64::MAX)),
    })
}

fn one_pass<E: Engine>(
    mut engine: E,
    lines: &[String],
    blocks: &[ReplayBlock],
    snapshot: u64,
    window: u64,
) -> Result<Pass, TimingError> {
    let mut gate = WarmGate::new(GateConfig {
        steps: vec!["fresh-engine"],
        min_window_ns: 0,
        min_window_events: window,
    });
    let _ = gate.pass("fresh-engine", 0);
    let mut pass = Pass {
        replay: Replay::starting_at(snapshot),
        ..Pass::default()
    };
    for (i, (line, rb)) in lines.iter().zip(blocks).enumerate() {
        let t_parse = Instant::now();
        let block =
            hl_block::decode_block(line).map_err(|e| TimingError::Decode(i + 1, e.to_string()))?;
        if block.time_ns != rb.block().time_ns || block.round != rb.block().round {
            return Err(TimingError::Mismatch {
                index: i,
                height: rb.height(),
                line_ns: block.time_ns,
                block_ns: rb.block().time_ns,
            });
        }
        let acts: Vec<UserAction<'_>> = block.actions().map(UserAction::from_signed).collect();
        let t_exec = Instant::now();
        let out = engine.execute_block(block.time_ns, &acts);
        let exec_ns = ns(t_exec);
        let parse_exec_ns = ns(t_parse);
        drop(acts);
        let phase = gate.on_event(0);
        let before = pass.replay.reproduced;
        pass.replay.record(rb, &out, &mut engine);
        let reproduced = pass.replay.reproduced > before;
        if phase == Phase::Warmup {
            pass.excluded_warmup += 1;
        } else if reproduced {
            pass.exec_ns.push(exec_ns);
            pass.parse_exec_ns.push(parse_exec_ns);
        }
        pass.rows.push(Row {
            height: rb.height(),
            phase,
            reproduced,
            actions: u32::try_from(rb.native_responses().len()).unwrap_or(u32::MAX),
            exec_ns,
            parse_exec_ns,
        });
    }
    pass.reproduced = pass.replay.reproduced;
    Ok(pass)
}

/// Integer nearest-rank percentile of sorted `v`, `per_mille` in 1..=1000.
#[must_use]
pub fn percentile(sorted: &[u64], per_mille: u64) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    let n = u64::try_from(sorted.len()).ok()?;
    let rank = (per_mille * n).div_ceil(1000).max(1);
    sorted.get(usize::try_from(rank - 1).ok()?).copied()
}

/// One column of the table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Column {
    /// Median over all counted samples.
    pub p50: u64,
    /// p90.
    pub p90: u64,
    /// p99.
    pub p99: u64,
    /// Smallest per-pass maximum.
    pub worst_min: u64,
    /// Largest per-pass maximum.
    pub worst_max: u64,
    /// Integer mean (requested by the report format; never the headline).
    pub mean: u64,
}

fn column(passes: &[Pass], pick: impl Fn(&Pass) -> &Vec<u64>) -> Column {
    let mut all: Vec<u64> = passes
        .iter()
        .flat_map(|p| pick(p).iter().copied())
        .collect();
    all.sort_unstable();
    let maxes: Vec<u64> = passes
        .iter()
        .filter_map(|p| pick(p).iter().max().copied())
        .collect();
    let n = u128::try_from(all.len()).unwrap_or(1).max(1);
    let sum: u128 = all.iter().map(|&x| u128::from(x)).sum();
    Column {
        p50: percentile(&all, 500).unwrap_or(0),
        p90: percentile(&all, 900).unwrap_or(0),
        p99: percentile(&all, 990).unwrap_or(0),
        worst_min: maxes.iter().min().copied().unwrap_or(0),
        worst_max: maxes.iter().max().copied().unwrap_or(0),
        mean: u64::try_from(sum / n).unwrap_or(u64::MAX),
    }
}

/// The table's numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Summary {
    /// Counted samples (all passes).
    pub samples: u64,
    /// Passes.
    pub passes: u64,
    /// `execute_block` only.
    pub exec: Column,
    /// Decode + `execute_block`.
    pub parse_exec: Column,
    /// (blocks reproduced in the worst pass, blocks).
    pub state_identical: (u64, u64),
}

impl Timed {
    /// Pools the passes into the report table.
    #[must_use]
    pub fn summary(&self) -> Summary {
        Summary {
            samples: self
                .passes
                .iter()
                .map(|p| u64::try_from(p.exec_ns.len()).unwrap_or(0))
                .sum(),
            passes: u64::try_from(self.passes.len()).unwrap_or(0),
            exec: column(&self.passes, |p| &p.exec_ns),
            parse_exec: column(&self.passes, |p| &p.parse_exec_ns),
            state_identical: self.state_identical,
        }
    }

    /// Per-block CSV over all counted passes.
    #[must_use]
    pub fn to_csv(&self) -> String {
        let mut s = String::from("pass,height,phase,reproduced,actions,exec_ns,parse_exec_ns\n");
        for (i, p) in self.passes.iter().enumerate() {
            for r in &p.rows {
                let ph = if r.phase == Phase::Measured {
                    "measured"
                } else {
                    "warmup"
                };
                let _ = writeln!(
                    s,
                    "{i},{},{ph},{},{},{},{}",
                    r.height,
                    u8::from(r.reproduced),
                    r.actions,
                    r.exec_ns,
                    r.parse_exec_ns
                );
            }
        }
        s
    }
}

impl Summary {
    /// The reference table, ns.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(
            s,
            "samples {} (reproduced, gate-open blocks over {} passes)",
            self.samples, self.passes
        );
        let _ = writeln!(
            s,
            "{:<22}{:>16}{:>18}",
            "ns", "execute_block", "parse+execute"
        );
        let row = |s: &mut String, name: &str, a: u64, b: u64| {
            let _ = writeln!(s, "{name:<22}{a:>16}{b:>18}");
        };
        row(&mut s, "median", self.exec.p50, self.parse_exec.p50);
        row(&mut s, "p90", self.exec.p90, self.parse_exec.p90);
        row(&mut s, "p99", self.exec.p99, self.parse_exec.p99);
        let _ = writeln!(
            s,
            "{:<22}{:>16}{:>18}",
            "worst (range/pass)",
            format!("{}..{}", self.exec.worst_min, self.exec.worst_max),
            format!(
                "{}..{}",
                self.parse_exec.worst_min, self.parse_exec.worst_max
            )
        );
        row(&mut s, "mean", self.exec.mean, self.parse_exec.mean);
        let _ = writeln!(
            s,
            "state identical {}/{}",
            self.state_identical.0, self.state_identical.1
        );
        s
    }
}
