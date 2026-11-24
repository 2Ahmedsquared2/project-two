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

#[derive(Clone, Debug, PartialEq, Eq)]
enum StepState {
    Pending(Option<String>),
    Passed(u64),
    Failed(String),
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

/// Closed until every step passed and the window elapsed; never closes again.
#[derive(Clone, Debug)]
pub struct WarmGate {
    cfg: GateConfig,
    state: Vec<StepState>,
    armed_at: Option<u64>,
    window_events: u64,
    warmup_events: u64,
    opened_at: Option<u64>,
}

impl WarmGate {
    /// A closed gate.
    #[must_use]
    pub fn new(cfg: GateConfig) -> Self {
        let state = vec![StepState::Pending(None); cfg.steps.len()];
        Self {
            cfg,
            state,
            armed_at: None,
            window_events: 0,
            warmup_events: 0,
            opened_at: None,
        }
    }

    fn idx(&self, step: &str) -> Result<usize, GateError> {
        self.cfg
            .steps
            .iter()
            .position(|s| *s == step)
            .ok_or_else(|| GateError::UnknownStep(step.to_owned()))
    }

    /// Marks `step` passed at `now_ns`. Arms the window when it is the last.
    pub fn pass(&mut self, step: &str, now_ns: u64) -> Result<(), GateError> {
        let i = self.idx(step)?;
        if !matches!(self.state[i], StepState::Pending(_)) {
            return Err(GateError::Settled(step.to_owned()));
        }
        self.state[i] = StepState::Passed(now_ns);
        if self.state.iter().all(|s| matches!(s, StepState::Passed(_))) {
            self.armed_at = Some(now_ns);
        }
        Ok(())
    }

    /// Marks `step` failed: the gate can then never open.
    pub fn fail(&mut self, step: &str, why: &str, _now_ns: u64) {
        if let Ok(i) = self.idx(step) {
            self.state[i] = StepState::Failed(why.to_owned());
        }
    }

    /// Attaches a note to a pending step (e.g. why it is still pending).
    pub fn note(&mut self, step: &str, note: &str) {
        if let Ok(i) = self.idx(step) {
            if let StepState::Pending(n) = &mut self.state[i] {
                *n = Some(note.to_owned());
            }
        }
    }

    /// Classifies one event at `now_ns` and advances the window.
    pub fn on_event(&mut self, now_ns: u64) -> Phase {
        if self.opened_at.is_some() {
            return Phase::Measured;
        }
        if let Some(armed) = self.armed_at {
            if self.window_events >= self.cfg.min_window_events
                && now_ns.saturating_sub(armed) >= self.cfg.min_window_ns
            {
                self.opened_at = Some(now_ns);
                return Phase::Measured;
            }
            self.window_events += 1;
        }
        self.warmup_events += 1;
        Phase::Warmup
    }

    /// Whether the gate is open.
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

    /// One line for the gate, then one per step in configured order.
    #[must_use]
    pub fn report(&self) -> String {
        let mut s = String::new();
        let _ = match self.opened_at {
            Some(t) => writeln!(
                s,
                "gate OPEN at {t} ns after {} warm-up events",
                self.warmup_events
            ),
            None => writeln!(s, "gate CLOSED after {} warm-up events", self.warmup_events),
        };
        for (name, st) in self.cfg.steps.iter().zip(&self.state) {
            let _ = match st {
                StepState::Passed(t) => writeln!(s, "{name} passed at {t} ns"),
                StepState::Failed(w) => writeln!(s, "{name} FAILED: {w}"),
                StepState::Pending(None) => writeln!(s, "{name} PENDING"),
                StepState::Pending(Some(n)) => writeln!(s, "{name} PENDING ({n})"),
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
