//! The warm gate and the histogram that respects it.

use std::fmt::Write as _;

use hdrhistogram::Histogram;

/// Whether a sample counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Taken before the gate opened: excluded from every reported number.
    Warmup,
    /// Taken with the gate open.
    Measured,
}

/// What the gate requires.
#[derive(Clone, Debug)]
pub struct GateConfig {
    /// Steps that must all pass, in report order.
    pub steps: Vec<&'static str>,
    /// Time that must elapse after the last step passed.
    pub min_window_ns: u64,
    /// Events that must be seen after the last step passed. The gate opens on
    /// the first event after both minimums are met; that event is measured.
    pub min_window_events: u64,
}

/// Why a step transition was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GateError {
    /// Not one of the configured steps.
    #[error("unknown warm-up step {0:?}")]
    UnknownStep(String),
    /// Already passed or failed.
    #[error("warm-up step {0:?} already settled")]
    Settled(String),
}

/// The gate's decision logic: no allocation, at most 64 steps by index.
/// [`WarmGate`] adds step names and notes around it. Closed until every step
/// passed, then until both window minimums have elapsed after the last pass;
/// a failed step keeps it closed for good; once open it stays open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GateCore {
    steps: u32,
    passed: u64,
    failed: u64,
    min_window_ns: u64,
    min_window_events: u64,
    armed_at: Option<u64>,
    window_events: u64,
    warmup_events: u64,
    opened_at: Option<u64>,
}

/// Why a [`GateCore`] step transition was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreError {
    /// Step index out of range.
    Unknown,
    /// Already passed or failed.
    Settled,
}

impl GateCore {
    /// A closed gate over `steps` steps (clamped to 64).
    #[must_use]
    pub fn new(steps: u32, min_window_ns: u64, min_window_events: u64) -> Self {
        Self {
            steps: steps.min(64),
            passed: 0,
            failed: 0,
            min_window_ns,
            min_window_events,
            armed_at: None,
            window_events: 0,
            warmup_events: 0,
            opened_at: None,
        }
    }

    fn all(&self) -> u64 {
        if self.steps == 64 {
            u64::MAX
        } else {
            (1u64 << self.steps) - 1
        }
    }

    /// Marks step `i` passed at `now_ns`; arms the window when it is the last.
    pub fn pass(&mut self, i: u32, now_ns: u64) -> Result<(), CoreError> {
        if i >= self.steps {
            return Err(CoreError::Unknown);
        }
        let bit = 1u64 << i;
        if (self.passed | self.failed) & bit != 0 {
            return Err(CoreError::Settled);
        }
        self.passed |= bit;
        if self.passed == self.all() {
            self.armed_at = Some(now_ns);
        }
        Ok(())
    }

    /// Marks step `i` failed (no effect on an out-of-range index). Disarms the
    /// window, so the gate stays closed for good unless it already opened.
    pub fn fail(&mut self, i: u32) {
        if i < self.steps {
            let bit = 1u64 << i;
            self.passed &= !bit;
            self.failed |= bit;
            self.armed_at = None;
        }
    }

    /// Classifies one event at `now_ns` and advances the window.
    pub fn on_event(&mut self, now_ns: u64) -> Phase {
        if self.opened_at.is_some() {
            return Phase::Measured;
        }
        if let Some(armed) = self.armed_at {
            if self.window_events >= self.min_window_events
                && now_ns.saturating_sub(armed) >= self.min_window_ns
            {
                self.opened_at = Some(now_ns);
                return Phase::Measured;
            }
            self.window_events += 1;
        }
        self.warmup_events += 1;
        Phase::Warmup
    }

    /// Open.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.opened_at.is_some()
    }

    /// When it opened.
    #[must_use]
    pub fn opened_at_ns(&self) -> Option<u64> {
        self.opened_at
    }

    /// Events classified as warm-up.
    #[must_use]
    pub fn warmup_events(&self) -> u64 {
        self.warmup_events
    }

    /// Whether step `i` passed / failed.
    #[must_use]
    pub fn step(&self, i: u32) -> (bool, bool) {
        if i >= self.steps {
            return (false, false);
        }
        let bit = 1u64 << i;
        (self.passed & bit != 0, self.failed & bit != 0)
    }
}

/// Closed until every step passed and the window elapsed; never closes again.
#[derive(Clone, Debug)]
pub struct WarmGate {
    names: Vec<&'static str>,
    core: GateCore,
    passed_at: Vec<u64>,
    notes: Vec<Option<String>>,
}

impl WarmGate {
    /// A closed gate.
    #[must_use]
    pub fn new(cfg: GateConfig) -> Self {
        let n = cfg.steps.len();
        let mut core = GateCore::new(
            u32::try_from(n.min(64)).unwrap_or(64),
            cfg.min_window_ns,
            cfg.min_window_events,
        );
        let mut notes = vec![None; n];
        if n > 64 {
            // Fail closed: GateCore tracks 64 steps; dropping the rest would
            // open the gate with required steps still pending.
            core.fail(0);
            notes[0] = Some(format!(
                "gate configured with {n} steps, more than 64 supported: never opens"
            ));
        }
        Self {
            names: cfg.steps,
            core,
            passed_at: vec![0; n],
            notes,
        }
    }

    fn idx(&self, step: &str) -> Result<usize, GateError> {
        self.names
            .iter()
            .position(|s| *s == step)
            .ok_or_else(|| GateError::UnknownStep(step.to_owned()))
    }

    /// Marks `step` passed at `now_ns`. Arms the window when it is the last.
    pub fn pass(&mut self, step: &str, now_ns: u64) -> Result<(), GateError> {
        let i = self.idx(step)?;
        if i >= 64 {
            return Err(GateError::UnknownStep(step.to_owned()));
        }
        let ci = u32::try_from(i).map_err(|_| GateError::UnknownStep(step.to_owned()))?;
        self.core
            .pass(ci, now_ns)
            .map_err(|_| GateError::Settled(step.to_owned()))?;
        self.passed_at[i] = now_ns;
        Ok(())
    }

    /// Marks `step` failed: the gate can then never open.
    pub fn fail(&mut self, step: &str, why: &str, _now_ns: u64) {
        if let Ok(i) = self.idx(step) {
            if let Ok(ci) = u32::try_from(i) {
                self.core.fail(ci);
            }
            self.notes[i] = Some(why.to_owned());
        }
    }

    /// Attaches a note to a pending step (e.g. why it is still pending).
    pub fn note(&mut self, step: &str, note: &str) {
        if let Ok(i) = self.idx(step) {
            if let Ok(ci) = u32::try_from(i) {
                if self.core.step(ci) == (false, false) {
                    self.notes[i] = Some(note.to_owned());
                }
            }
        }
    }

    /// Classifies one event at `now_ns` and advances the window.
    pub fn on_event(&mut self, now_ns: u64) -> Phase {
        self.core.on_event(now_ns)
    }

    /// Whether the gate is open.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.core.is_open()
    }

    /// When it opened.
    #[must_use]
    pub fn opened_at_ns(&self) -> Option<u64> {
        self.core.opened_at_ns()
    }

    /// Events classified as warm-up.
    #[must_use]
    pub fn warmup_events(&self) -> u64 {
        self.core.warmup_events()
    }

    /// One line for the gate, then one per step in configured order.
    #[must_use]
    pub fn report(&self) -> String {
        let mut s = String::new();
        let _ = match self.core.opened_at_ns() {
            Some(t) => writeln!(
                s,
                "gate OPEN at {t} ns after {} warm-up events",
                self.core.warmup_events()
            ),
            None => writeln!(
                s,
                "gate CLOSED after {} warm-up events",
                self.core.warmup_events()
            ),
        };
        for (i, name) in self.names.iter().enumerate() {
            let (passed, failed) = self.core.step(u32::try_from(i).unwrap_or(u32::MAX));
            let note = self.notes[i].as_deref();
            let _ = match (passed, failed, note) {
                (true, _, _) => writeln!(s, "{name} passed at {} ns", self.passed_at[i]),
                (_, true, w) => writeln!(s, "{name} FAILED: {}", w.unwrap_or("")),
                (_, _, None) => writeln!(s, "{name} PENDING"),
                (_, _, Some(n)) => writeln!(s, "{name} PENDING ({n})"),
            };
        }
        s
    }
}

/// An HDR histogram (1 ns .. 60 s, 3 significant digits) that drops samples
/// taken while the gate was closed, and counts them.
#[derive(Clone, Debug)]
pub struct GatedHistogram {
    measured: Histogram<u64>,
    excluded: u64,
}

impl GatedHistogram {
    /// Empty.
    pub fn new() -> Result<Self, hdrhistogram::CreationError> {
        Ok(Self {
            measured: Histogram::new_with_bounds(1, 60_000_000_000, 3)?,
            excluded: 0,
        })
    }

    /// Records `value_ns` when `phase` is measured; counts it as excluded otherwise.
    pub fn record(&mut self, phase: Phase, value_ns: u64) -> Result<(), hdrhistogram::RecordError> {
        match phase {
            Phase::Warmup => {
                self.excluded += 1;
                Ok(())
            }
            Phase::Measured => self.measured.record(value_ns.max(1)),
        }
    }

    /// The measured samples.
    #[must_use]
    pub fn measured(&self) -> &Histogram<u64> {
        &self.measured
    }

    /// Samples dropped as warm-up.
    #[must_use]
    pub fn excluded(&self) -> u64 {
        self.excluded
    }
}

// Kani harnesses for `GateCore` (src/gate.rs: `GateCore::new`, `pass`, `fail`,
// `on_event`, `is_open`; `WarmGate` delegates every decision to it). Source
// read at 97f85f2 plus this commit. Bounds: 2 or 3 steps (u32), up to 6
// operations per harness, each pass(i, t) / fail(i) / on_event(t) with
// i: u32 < 4 (so out-of-range indexes are included), t: u64 < 16,
// min_window_ns: u64 < 8, min_window_events: u64 < 4. No stubs. The ghost
// state is written from the documented contract, not copied from the code.
#[cfg(kani)]
mod proofs {
    use super::*;

    /// Soundness: the first `Measured` event happens only if every step passed
    /// and none failed, at least `min_window_events` events were classified
    /// since the last pass, and at least `min_window_ns` elapsed since it.
    /// Once `Measured`, always `Measured`; `is_open` agrees.
    #[kani::proof]
    #[kani::unwind(8)]
    fn opens_only_when_every_step_passed_and_window_elapsed() {
        let n: u32 = if kani::any() { 2 } else { 3 };
        let min_ns: u64 = kani::any();
        let min_ev: u64 = kani::any();
        kani::assume(min_ns < 8 && min_ev < 4);
        let mut g = GateCore::new(n, min_ns, min_ev);
        let mut passed = [false; 4];
        let mut failed = [false; 4];
        let mut armed: Option<u64> = None;
        let mut since: u64 = 0;
        let mut open = false;
        for _ in 0..6 {
            let op: u8 = kani::any();
            let i: u32 = kani::any();
            let t: u64 = kani::any();
            kani::assume(op < 3 && i < 4 && t < 16);
            let k = i as usize;
            if op == 0 {
                let ok = g.pass(i, t).is_ok();
                assert_eq!(ok, i < n && !passed[k] && !failed[k]);
                if ok {
                    passed[k] = true;
                    if (0..n as usize).all(|j| passed[j]) {
                        armed = Some(t);
                        since = 0;
                    }
                }
            } else if op == 1 {
                g.fail(i);
                if i < n {
                    passed[k] = false;
                    failed[k] = true;
                    armed = None;
                }
            } else {
                let p = g.on_event(t);
                if open {
                    assert!(p == Phase::Measured);
                } else if p == Phase::Measured {
                    assert!((0..n as usize).all(|j| passed[j] && !failed[j]));
                    assert!(armed.is_some());
                    assert!(since >= min_ev);
                    assert!(t.saturating_sub(armed.unwrap_or(0)) >= min_ns);
                    open = true;
                } else if armed.is_some() {
                    since += 1;
                }
                kani::cover!(p == Phase::Measured && n == 3 && min_ev == 1 && min_ns == 1);
            }
            assert_eq!(g.is_open(), open);
        }
    }

    /// A failed step keeps the gate closed for every later event, even when it
    /// failed after the window was armed.
    #[kani::proof]
    #[kani::unwind(8)]
    fn a_failed_step_keeps_it_closed() {
        let min_ns: u64 = kani::any();
        let min_ev: u64 = kani::any();
        kani::assume(min_ns < 8 && min_ev < 4);
        let mut g = GateCore::new(2, min_ns, min_ev);
        let f: u32 = kani::any();
        kani::assume(f < 2);
        let fail_after_arming: bool = kani::any();
        if fail_after_arming {
            assert!(g.pass(0, 0).is_ok());
            assert!(g.pass(1, 0).is_ok());
            g.fail(f);
        } else {
            g.fail(f);
            let _ = g.pass(0, 0);
            let _ = g.pass(1, 0);
        }
        for _ in 0..5 {
            let t: u64 = kani::any();
            kani::assume(t < 16);
            assert!(g.on_event(t) == Phase::Warmup);
        }
        kani::cover!(fail_after_arming);
        assert!(!g.is_open());
    }

    /// Liveness within the bound: once every step passed at `t0`, the gate
    /// opens on the first event with at least `min_window_events` earlier
    /// window events and `t - t0 >= min_window_ns`, and not before.
    #[kani::proof]
    #[kani::unwind(8)]
    fn opens_on_the_first_event_that_meets_both_minimums() {
        let min_ns: u64 = kani::any();
        let min_ev: u64 = kani::any();
        kani::assume(min_ns < 8 && min_ev < 4);
        let mut g = GateCore::new(2, min_ns, min_ev);
        let t0: u64 = kani::any();
        kani::assume(t0 < 8);
        assert!(g.pass(1, t0).is_ok());
        assert!(g.pass(0, t0).is_ok());
        let mut seen = 0u64;
        let mut last = t0;
        for _ in 0..5 {
            let t: u64 = kani::any();
            kani::assume(t >= last && t < 16);
            last = t;
            let p = g.on_event(t);
            if seen >= min_ev && t - t0 >= min_ns {
                assert!(p == Phase::Measured);
                kani::cover!(seen == 3 && min_ns == 7);
                return;
            }
            assert!(p == Phase::Warmup);
            seen += 1;
        }
    }
}
