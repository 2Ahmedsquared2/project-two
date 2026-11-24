# Lane 3 proof evidence: `hl-warm` warm gate

Tool: Kani 0.68.0 (CBMC with CaDiCaL), run through the workspace
`formal-verify` skill's `kani-check.sh` and `mutate.py` (GPL-3.0 tooling, run
from outside the repository, not copied in).

**What a Kani pass means**: Kani checked every input within the stated bounds
and found no violation. Larger inputs are not covered.

**What a mutation result means**: deliberate bugs were planted in copies of the
production code, and the checks caught each one.

## Target

`GateCore` in `crates/hl-warm/src/gate.rs` (`new`, `pass`, `fail`, `on_event`,
`is_open`). `WarmGate`, the type the node uses, keeps names and notes and
delegates every decision to `GateCore`. The harnesses call the production
functions; nothing is transcribed. The expected behaviour in the harnesses
(ghost state) is written from the documented contract.

## Properties (harnesses in `#[cfg(kani)] mod proofs`, same file)

1. `opens_only_when_every_step_passed_and_window_elapsed` (soundness): the
   first event classified `Measured` happens only if every step passed and
   none failed, at least `min_window_events` events were classified since the
   last pass, and at least `min_window_ns` elapsed since it; once `Measured`,
   every later event is `Measured`; `is_open()` agrees at every step; `pass`
   succeeds exactly for an in-range, not-yet-settled step.
2. `a_failed_step_keeps_it_closed`: a failed step (before or after the window
   armed) keeps every later event `Warmup`.
3. `opens_on_the_first_event_that_meets_both_minimums` (liveness within the
   bound): after all steps passed at `t0`, the gate opens on the first event
   with at least `min_window_events` earlier window events and
   `t - t0 >= min_window_ns`, and not before.

## Bounds

2 or 3 steps (`u32`); up to 6 operations per run (pass / fail / event, in any
order) in harness 1, 5 events in harnesses 2 and 3; step index `u32 < 4`
(includes out-of-range); times `u64 < 16`; `min_window_ns: u64 < 8`;
`min_window_events: u64 < 4`; unwind 8. No stubs, no assumptions beyond these
ranges (and non-decreasing event times in harness 3).

## Commands and output (run at the commit that adds this file)

```
bash <formal-verify>/scripts/kani-check.sh <repo>/crates/hl-warm
PASS gate::proofs::a_failed_step_keeps_it_closed unwind=8 checks=213
PASS gate::proofs::opens_on_the_first_event_that_meets_both_minimums unwind=8 checks=190
PASS gate::proofs::opens_only_when_every_step_passed_and_window_elapsed unwind=8 checks=344
SUMMARY 3 of 3 harnesses passed

python3 <formal-verify>/scripts/mutate.py <repo>/crates/hl-warm/src/gate.mutations
DETECTED fail does not disarm the window (the bug the regression test reproduced)
DETECTED open on the event count alone, ignoring time
DETECTED open one event early
DETECTED arm before the last step passed
DETECTED a failed step can be passed later
DETECTED open one event late (liveness)
SUMMARY 6 of 6 mutations detected
```

Every harness has a `kani::cover!` witness that the checker requires to be
`Satisfied` (the gate actually opens with non-zero minimums; a failure after
arming is reached; the latest opening point is reached).

## Bug found

Writing harness 2 exposed a real bug: `fail()` on a step that had already
passed, after the window armed, left the window armed, so the gate could still
open. Reproduced by `tests/gate.rs::failing_a_passed_step_after_arming_keeps_it_closed`
(failed before the fix, passes after); fixed by disarming in `fail`. The
first mutation replays that bug and is detected.

## Not covered

- Step counts above 3, sequences longer than 6 operations, larger times or
  window minimums (bounded check, not a proof for all sizes; a Lean proof would
  be needed for that).
- `WarmGate`'s name lookup, notes and report text (tested by unit tests only).
- `GatedHistogram`, `Arena`, pinning, `mlockall`, `getrusage`: OS-facing,
  tested on the dev box only, not proved.
- Thread interleavings: the gate is single-threaded by design (owned by the
  hot thread); no TLA+ model. TLC was not run (no Java runtime on the dev box).
- Nothing here is about speed.
