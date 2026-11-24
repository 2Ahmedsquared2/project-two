# TLA+ model of the p2-node replay driver

`ReplayDriver.tla` models the replay driver from lane 3, draft PR #10, branch
`reason/p2-lane-3-p2-node-integr-5178d817`, modelled at commit
`dc993fc1eb6d1510c2415a478093e4f2dc0452e2` (the sync commit). It is checked with
TLC on small finite instances.

This is a correctness model of our driver only. It says nothing about speed and
nothing about equivalence to native `hl-node`. A pass means the property holds
for this model, within the bounds and assumptions below.

## Run

```bash
formal/replay/run.sh          # needs Java 11+ on PATH (checked with Temurin 17.0.21.1)
```

`run.sh` downloads `tla2tools.jar` **v1.7.4** (the latest non-prerelease
release; v1.8.0 is a prerelease) and checks its sha256
`936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`. It then
runs every `cfg/*.cfg` and checks the outcome listed in `expected.txt`:
`pass` or `violated`. It exits non-zero if any run ends differently. Set
`TLA2TOOLS_JAR` to reuse a jar, or `TLC_LOG_DIR` to keep the TLC logs. One run
takes about 90 s on a small cloud machine (the largest config has 2.37 M
distinct states).

The repo has no `proofs/manifest.toml`, so this README holds the run command.
Cache key for the team proof cache: sha256 over `formal/replay/**` plus the
covered files listed below.

## What is modelled

The driver walks a slice of `ReplayBlock`s in slice order. For each block it calls
`engine.execute_block`, then `Replay::record`. `record` does `blocks += 1`, calls
`compare(b, out)` (our per-action responses and book diffs against native's),
then either does `reproduced += 1` or calls `engine.resync(height, native_diffs)`.

Books are not modelled as data. `synced` means our book equals native's book.
For each block the engine is right or wrong, as a free choice, so every
pattern of engine bugs is explored. If our book already differed from native's
before a block, that block's outputs may still equal native's (the difference
sits somewhere the block does not touch), but our book stays different.

`FIX = FALSE` is the code as written. `FIX = TRUE` is the fix proposed to lane 3:
refuse a block whose height is not `last + 1`, and stop when native's diffs for a
block are missing.

### Action to source map (commit dc993fc)

| Model action | Code |
|---|---|
| `Init` (engine built from native state at `snapH`; `synced = TRUE`) | fresh engine per pass: `crates/p2-node/src/timing.rs:119`, `:123`; snapshot loading is the caller's job and is not in the driver |
| `Init` (`input`, `hasDiffs`) | slice built by the caller with `ReplayBlock::new` `crates/p2-node/src/replay.rs:54-78`; diffs looked up by height, `None` when absent (`replay.rs:60-70`) |
| `Execute` | `for b in blocks` / `engine.execute_block` `replay.rs:245-247`; timed loop `timing.rs:145`, `:161` |
| `RejectHeight` (fix only) | not in code (proposed) |
| `Compare` | `record`: `blocks += 1` `replay.rs:255`; `compare(b, out)` `replay.rs:264` -> `replay.rs:182-239`; `NoNativeDiffs` `replay.rs:219-225`; `reproduced += 1` `replay.rs:266-267` |
| `Reproduced` | `None =>` arm `replay.rs:265-268`; `timing.rs:168` |
| `Resync` | `Some(m) =>` arm, `engine.resync(b.height, b.native_diffs())` `replay.rs:269-275`; `native_diffs()` returns `&[]` when absent `replay.rs:100-102`; contract `crates/p2-node/src/engine.rs:53-56` |
| `HaltNoDiffs` (fix only) | not in code (proposed) |
| `ResyncEarly`, `StopOnMismatch` | mutations only |
| k / N | `Replay.reproduced` / `Replay.blocks` `replay.rs:161-163`; `state_identical = (min reproduced, blocks)` `timing.rs:94`, `:125-128` |

Covered files: `crates/p2-node/src/replay.rs`, `crates/p2-node/src/timing.rs`,
`crates/p2-node/src/engine.rs`. If any of them changes, re-read the model
against the code before moving the sync commit.

## Properties

| Id | Property (TLA+ name) | Harm if broken |
|---|---|---|
| P1 | Each block is applied exactly once, in height order: each applied block is exactly one above the previous applied block or the snapshot (`P1_ExactlyOnceInOrder`). On a well-formed slice every block is applied (`P1_AllApplied`, liveness). | Engine state built from a skipped, repeated or reordered block, so every later verdict is about the wrong state. |
| P2 | A snapshot at height H is never followed by a block at or below H (`P2_NoBlockAtOrBelowSnapshot`). | A block applied twice on top of a state that already contains it. |
| P3 | A block's verdict is fixed before the driver resyncs to native's book (`P3_VerdictBeforeResync`), it is the verdict of our own execution (`P3_VerdictIsOurs`), and it never changes afterwards (`P3_VerdictNeverChanges`). | Resync-then-compare always "matches", so k/N would report native's book as ours. |
| P4a | A block whose outputs (responses + book diffs) differ from native's is never counted as reproduced, and k = \|counted\| <= N (`P4a_OutputMismatchNeverCounted`). | Inflated k/N. |
| P4b | A block counted as reproduced also left our book equal to native's (`P4b_CountedImpliesStateIdentical`). The report calls k/N `state_identical`. | k/N reported as state-identical while our book has drifted. |

## Results (tla2tools v1.7.4, TLC 2.19, MaxH = 4, MaxLen = 4)

| Config | Model | Expected | Result |
|---|---|---|---|
| `code_wellformed` | code; slice = snapH+1..MaxH; native diffs for every block | pass, all of P1-P4b + liveness | pass (160 states) |
| `code_any_input_holds` | code; any slice; diffs may be missing | P3, P4a pass | pass (2,371,200 states) |
| `code_missing_diffs_P4a` | code; well-formed slice; diffs may be missing | P1, P2, P4a pass | pass |
| `code_any_input_P1` | code; any slice | **F1**: P1 violated | violated |
| `code_any_input_P2` | code; any slice | **F1**: P2 violated | violated |
| `code_missing_diffs_P4b` | code; well-formed slice; diffs may be missing | **F2**: P4b violated | violated |
| `fix_safety` | proposed fix; any slice; diffs may be missing | all safety pass | pass (83,104 states) |
| `fix_live` | proposed fix; well-formed slice | P1 liveness passes | pass |

So the code at dc993fc satisfies P1-P4b only under two preconditions on the
caller: (a) the slice is exactly heights H+1, H+2, ... where H is the height of
the state the engine was built from, and (b) native's diffs are present for
every block. P3 and P4a hold for any input.

### Findings (sent to lane 3 as bug reports; this PR does not change their code)

- **F1, P1 and P2.** `Replay::run` (`replay.rs:243-251`) and `one_pass`
  (`timing.rs:145-183`) never compare `b.height()` with the previous block or with
  the height of the engine's starting state. TLC's shortest traces: (P1) engine
  at genesis, slice `[2]`: block 2 is applied with block 1 missing. (P2) engine
  built from the snapshot at 1, slice `[1]`: block 1 is applied on a state that
  already contains it. The same holds for a repeated line (`[h, h]`, for example
  overlapping `replica_cmds` files) or swapped lines. The block-time alignment
  check in `ReplayBlock::new` (`replay.rs:54-70`) does not catch these, because
  each block keeps its own height and diffs. Proposed fix (modelled,
  `fix_safety` passes): reject any block whose height is not `last + 1`, and
  start `last` at the snapshot height.
- **F2, P4b.** When native's diffs for block h are missing, `compare` reports
  `NoNativeDiffs` and `record` calls `engine.resync(h, &[])`, because
  `native_diffs()` returns `&[]` when absent (`replay.rs:100-102`). Absent is
  not the same as empty: resync cannot restore native's changes. Trace: block 1
  has no native diffs and our engine gets it wrong, so it mismatches and
  `resync(1, &[])` leaves our book different from native's. Block 2 has diffs,
  our engine is right, and the difference sits where block 2 does not touch.
  Outputs match, so `reproduced += 1`, but our book differs from native's.
  `state_identical` (`timing.rs:125-128`) overstates. The existing test
  `missing_native_diffs_is_not_reproduced` uses a scripted engine that ignores
  resync, so it cannot see this. Proposed fix (modelled, `fix_safety` passes):
  stop with an error when native's diffs are missing. Alternatives are
  `resync(h, Option<&[BookDiff]>)` plus no longer counting later blocks as
  state-identical, or reloading a snapshot.

## Mutations (each property seen failing)

Each mutation is one model step changed to mirror a plausible code bug. Every one
is in `expected.txt` as `violated` and fails with the named property.

| Mutation (`MUTANT`) | Plausible code bug | Fails |
|---|---|---|
| `guard_gt` | height guard written as `h > last` (gaps pass) | P1 |
| `guard_ge` | guard off by one, `h >= last` (duplicates pass) | P1 |
| `stop_on_mismatch` | `return` on first mismatch instead of resync | P1 liveness |
| `cursor_ignores_snapshot` | height cursor starts at 0, not the snapshot height | P2 |
| `resync_before_compare` | resync moved above `compare` | P3 (`VerdictBeforeResync`, and `VerdictIsOurs`) |
| `recompare_after_resync` | block compared again after resync | P3 (`VerdictNeverChanges`) |
| `count_every_block` | `reproduced += 1` outside the `None` arm | P4a |
| `resync_without_diffs` | the fix's stop on missing diffs removed | P4b |

Vacuity probes (`probe_*`, all violated as expected) show the code model reaches
a resync, a snapshot followed by an applied block, a finished run with both a
mismatch and a reproduced block, and n > k.

## Bounds and assumptions

- MaxH = 4 heights, MaxLen = 4 blocks per slice, snapshot height 0..3. In the
  any-slice configs, every slice of length 1..4 over heights 1..4 is checked.
  Run once by hand at larger bounds (not in `run.sh`), all passing:
  `code_wellformed` and `fix_live` at MaxH = MaxLen = 7 (1,489 states),
  `code_missing_diffs_P4a` at 6/6 (68,487 states), and `fix_safety` at 5/5
  (2,139,040 states).
- A1. Heights are block identities: a `ReplayBlock`'s height matches its
  content. Mislabelled heights are partly covered by the block-time alignment
  check in `ReplayBlock::new` and are out of scope.
- A2. `engine.resync(h, d)`, given native's complete diffs for block h and a book
  that equalled native's before h, leaves our book equal to native's after h.
  This is the engine's contract (`engine.rs:53-56`); the model assumes it.
- A3. `engine.resync(h, &[])` either leaves our book as `execute_block` made it,
  or loses this block's changes. It never restores native's changes. Both
  choices are explored.
- A4. Block outputs (responses + book diffs) on an equal pre-block book equal
  native's exactly when the engine executed the block like native
  (`ok`). On a different pre-block book they may or may not match (`lucky`).
- Liveness uses weak fairness on every driver step.

## Not covered

- The engine itself (lane 1's `hl-exec`), `compare`'s field-by-field logic, JSON
  equality, and `hl-block` decoding.
- How the caller builds the slice (reading `replica_cmds` files, pairing them
  with a snapshot). F1 is exactly the gap between caller and driver.
- `one_pass` executes the block decoded from `lines[i]` but compares it with
  `blocks[i]` (`timing.rs:145-167`). Only the lengths are checked
  (`timing.rs:112-117`). The model assumes `lines[i]` decodes to
  `blocks[i].block`.
- Warm-up gating, timing, CSV output, and multiple passes (each pass is the same
  loop on a fresh engine).
- Anything about speed, and any claim of equivalence with native.
